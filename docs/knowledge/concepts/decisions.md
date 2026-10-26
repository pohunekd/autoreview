---
type: Concept
id: concept/decisions
title: Decisions and outcomes
description: How a finished review becomes one GitHub review - the outcome matrix of on_findings, on_clean and on_notes, blocking versus note findings, own pull requests, the approval wait, and the notes hand-off rules.
source_kind: manual
intents: [setup, help, debug]
---

# Decisions and outcomes

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

A finished review has a list of findings. The decision turns that list into
exactly one GitHub review event: `REQUEST_CHANGES`, `COMMENT` or `APPROVE`. Or,
for a clean review under `on_clean: await`, it turns the list into a wait for a
human. The rule that matched the pull request sets the outcome. The keys are
described in [policy](../../policy.md#repository-rules).

## Blocking and notes

Every finding is either **blocking** or a **note**. Only blocking findings count
toward the decision. Notes are published, but they never hold the merge back.

- A finding from any reviewer, whichever protocol it uses, is blocking only when
  the reviewer marks it `blocking` and its severity is `critical` or `high`. A
  finding marked blocking at `medium` or `low` is a note. Both protocols return
  the blocking flag (see [reviewers and adapters](reviewers-and-adapters.md#protocols)).
- A finding in a file the pull request does not change is a note (see
  [how it works](how-it-works.md#6-anchoring-findings-to-changed-lines)).
- The verifier can turn a blocking finding into a note, with the reason
  (see [verification](verification.md)).
- An open prior finding counts as blocking, even though no reviewer reported it
  again (see [incremental reviews](incremental-reviews.md)).

The **blocking count** of a review is the number of its blocking findings plus
its open prior findings. A review with a blocking count above zero is not clean.

## Outcome matrix

The matrix below uses the three rule keys. `on_notes` is only set together with
`on_clean: await` and `trigger: review-requested`, so its rows always carry that
clean outcome.

| Blocking count | `on_findings` | `on_clean` | `on_notes` | Notes present | Published event | Human needed |
|---|---|---|---|---|---|---|
| above 0 | `request_changes` | any | any | any | `REQUEST_CHANGES` | No |
| above 0 | `comment` | any | any | any | `COMMENT` | No |
| 0 | any | `approve` | not set | any | `APPROVE` | No |
| 0 | any | `approve` (escalated) | not set | any | wait, then `APPROVE` | Yes |
| 0 | any | `comment` | not set | any | `COMMENT` | No |
| 0 | any | `await` | not set | any | wait, then `APPROVE` | Yes |
| 0 | any | `await` | `comment` | no | wait, then `APPROVE` | Yes |
| 0 | any | `await` | `comment` | yes, first hand-off | `COMMENT` to the author, then wait | After the author asks again |
| 0 | any | `await` | `comment` | yes, after a hand-off | wait, then `APPROVE` | Yes |

An approval wait is only ever entered on a rule that never approves without a
human (`await`) or on an auto-approving rule whose verifier escalated the review.
Escalation is described in [verification](verification.md#escalation-on-auto-approve-rules).

## Precedence

The decision is taken in this order. The first rule that applies wins.

1. Blocking count above zero and `on_findings: request_changes`: `REQUEST_CHANGES`.
2. Blocking count above zero otherwise: `COMMENT`.
3. Clean and `on_clean: approve`: `APPROVE`, or a wait if the verifier escalated.
4. Clean with notes, `on_notes: comment`, no earlier hand-off for this head
   chain, the pull request not authored by the reviewing account, and the author
   not a bot: `COMMENT` to the author, marked as a hand-off.
5. Clean and `on_clean: await`: a wait for a human, then `APPROVE`.
6. Anything else: `COMMENT`.

After these rules, a pull request authored by the reviewing account is forced to
`COMMENT`, and any pending wait for it is dropped. See
[own pull requests](#own-pull-requests).

## Blocking versus note in the body

Blocking findings are inline comments where they can be anchored, and they are
listed in the body. Notes are listed in a collapsed section of the body with a
line that states the pull request needs no change for them. The hidden marker in
the body carries the decision and the number of blocking findings, so a reader
or a merge gate can tell a clean review from one with blocking findings without
parsing the text.

## Own pull requests

GitHub does not let an account approve or request changes on its own pull
request. autoreview therefore checks whether the pull request author is the
reviewing account, the login of the credential that serves the repository, and for
such a pull request:

- every outcome becomes `COMMENT`, whatever the rule says,
- no approval wait is created, even under `on_clean: await`,
- the notes hand-off does not apply,
- blocking findings raise the `findings_on_own_pr` event instead of
  `changes_requested`.

A review of an own pull request with no blocking findings raises `commented`.

The engine enforces this rule itself. It does not depend on GitHub rejecting the
request. See [GitHub identity](github-identity.md#the-own-pull-request-rule).

## Approval wait

A rule with `on_clean: await` ends a clean review in a wait. A rule with
`on_clean: approve` ends in a wait only when the verifier escalated it. The
wait works like this:

1. The review is recorded as waiting. The `approval_requested` event is sent as
   an `action` message, and the engine releases the pull request from
   scheduling. The record is written before the release, so discovery never sees
   the head without a review it keeps.
2. A person runs `autoreview approve --repository OWNER/REPO NUMBER HEAD_SHA`.
   The head SHA must be the full head commit of the pull request. The approval
   applies only if that commit is still the head.
3. The engine publishes the approval on its next tick. Until then the approval
   shows `publication_status: pending`. After publication it shows `published`
   with the GitHub review id.
4. Approving again is idempotent. The first decision is the one in effect.

The wait ends in one of three ways, and the first one that happens decides:

| Ending | Trigger | Event | Publication |
|---|---|---|---|
| Approved | `autoreview approve` with the current head | `approved` after publication | `APPROVE` |
| Superseded | A new head, or the trigger stops asking: the review request is removed, the label is removed, or the pull request is closed or turned back into a draft | `superseded` | None |
| Expired | `approval.timeout_hours` passed | `approval_expired` | None |

A renewed review request for the same head does not supersede the wait. Only a
new head or a trigger that stopped asking ends it.

When a head is no longer the pull request head, `approve` fails with
`head_changed`. A wait that already ended answers `review_superseded` or
`approval_expired`, and nothing is posted.

If the approved review cannot be published, the wait ends with a failure. The
engine raises `review_stopped` with a message that says the approved review could
not be published, and the review can be retried with `autoreview retry`, see
[retry and recovery](../runbooks/retry-and-recovery.md).

Approvals from a chat integration use the same CLI commands after
`autoreview resolve` has found the head. See
[approve from chat](../guides/hermes-approval.md) and
[human approval](../guides/human-approval.md).

## Notes hand-off

Notes hand-off is the optional step that gives the author the notes of a clean
review before a human approves anything. It is turned on with `on_notes: comment`
on a rule, which requires `on_clean: await` and `trigger: review-requested`. It
needs `notes_handoff.reminder_hours` in the policy.

The rules below describe the complete behaviour of the hand-off.

- **Hand-off.** A clean review with notes only, where the reviewing account is not
  the author and the author is not a bot, is published as a `COMMENT` to the
  author. The comment lists the notes. The review is remembered as a hand-off.
- **Renewed request, same head.** When the author asks for the review again at the
  same head, the engine does not run the reviewers again. It adopts the stored
  result of the hand-off review. The result is the same code sampled again
  otherwise, so the engine uses the stored result. The review then enters the
  approval wait, and `approval_requested` is raised.
- **New head after a hand-off.** A new head is reviewed. The review then waits for
  approval, whatever its notes are. A second hand-off is never made in a row.
  After a hand-off, only a human approval may follow.
- **Bot authors skip the hand-off.** A pull request whose author is a GitHub bot
  account (GraphQL type `Bot`) is not handed off. A review with only notes goes
  straight to the approval wait.
- **Own pull requests skip the hand-off.** See [own pull requests](#own-pull-requests).
- **Reminder.** When the author has not asked again within
  `notes_handoff.reminder_hours`, the user gets one `handoff_reminder` message.
  The reminder is sent once, and it is not repeated after that.
- **Renewed request stops the reminder.** A pull request that asks for a review
  again is no longer reminded. The next discovery pass reviews it.
- **Closed pull request ends the hand-off.** When the pull request is closed, the
  hand-off ends and no reminder is sent.
- **Unreadable state is retried.** A failed read of the pull request for the
  reminder is retried on the next pass. It holds up no review.

The timeline of one hand-off:

```text
review at head A:  notes only      -> COMMENT to author   (hand-off recorded)
author asks again at head A        -> approval wait       (stored result adopted)
author pushes head B               -> review of B         -> approval wait
```

## Decision table

| Situation | Decision | Notification |
|---|---|---|
| Blocking findings, `on_findings: request_changes`, not own PR | `REQUEST_CHANGES` | `changes_requested` (digest) |
| Blocking findings, `on_findings: comment`, not own PR | `COMMENT` | `commented` (digest) |
| Blocking findings, own PR | `COMMENT` | `findings_on_own_pr` (action) |
| Clean, `on_clean: approve`, not escalated | `APPROVE` | `approved` (digest) |
| Clean, `on_clean: approve`, escalated by verifier | Wait, then `APPROVE` on approval | `approval_requested` (action) |
| Clean, `on_clean: await` | Wait, then `APPROVE` on approval | `approval_requested` (action) |
| Clean, `on_clean: comment` | `COMMENT` | `commented` (digest) |
| Notes only, first time, `on_notes: comment` | `COMMENT` to author (hand-off) | `commented` (digest) |
| Hand-off, author asked again, same head | Wait, then `APPROVE` on approval | `approval_requested` (action) |
| Hand-off, no reply for `notes_handoff.reminder_hours` | None | `handoff_reminder` (action, once) |
| Own PR, clean | `COMMENT` | `commented` (digest) |
| Wait ends by a newer head or the trigger stopping | None | `superseded` (log) |
| Wait ends by timeout | None | `approval_expired` (digest) |

The level in brackets is the default in the example policy. Each deployment sets
its own level for every event in `notifications`.

## Related pages

- The keys that produce these rows: [policy](../../policy.md#repository-rules).
- Turning a clean review into an approval with a person in the loop: [human approval](../guides/human-approval.md).
- Notes that go to the author first: [notes hand-off](../guides/notes-handoff.md).
- Re-reviews that change the decision: [incremental reviews](incremental-reviews.md).
- The verifier that can escalate a review: [verification](verification.md).
- Event levels and delivery: [notifications](notifications.md).
