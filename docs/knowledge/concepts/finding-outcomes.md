---
type: Concept
id: concept/finding-outcomes
title: Finding outcomes
description: The record the engine keeps of every published finding, which outcome each finding gets from the verifier at publication, from later reviews and when its pull request is merged or closed, what happens when the record cannot be written, and how long the records stay.
source_kind: manual
intents: [help, debug]
---

# Finding outcomes

> **Status: after 0.3.0.** This page describes autoreview on `main` after the
> 0.3.0 release. A part that is not available names its open issue.

The engine records what became of every finding it publishes, so the quality of
the reviews can be measured without anyone rating findings by hand. The record
never changes a review: no decision reads it and nothing of it is written to
GitHub.

## What is recorded

When a review is published, each of its findings gets one row in the store,
blocking findings and notes alike. A row holds:

| Field | Value |
|---|---|
| Pull request | Repository and number |
| Finding | Its ID, and its key: the head SHA cut to 12 characters, a colon and the ID |
| Origin | The workflow ID of the review, the reviewer ID (the first reviewer of a merged finding), the pipeline and the rule `match` |
| Classification | Severity, confidence, path and line, and whether it blocked when it was published |
| Head SHAs | The head it was published on, and the head its outcome was decided at |
| Outcome | One of the outcomes below |

A finding that publishes an earlier one again (see
[incremental reviews](incremental-reviews.md)) gets no row of its own: it is
the earlier finding, whose row from then on follows the key of the new
finding, so the next review settles it. A notes hand-off that is approved later
publishes the same findings again, and their rows stay as they are.

Each review numbers its findings from `001`, so a later review of the same head
(a retarget, a new review request or a retry) can publish a finding whose key is
already stored. That finding adds no row: the first row stays as it is, and the
later finding has no row of its own. Unless the later review carried the first
finding as an earlier one, the first row loses its tracked key and stays
`open`, so no review settles it with the verdict on the other finding.

## Outcomes

| Outcome | Derived when |
|---|---|
| `open` | The finding is published and nothing has settled it |
| `refuted`, `not_blocking` | The [verifier](verification.md) demoted the blocking finding before it was published |
| `fixed` | A later published review settled the finding as `fixed`, or as `answered` and the verifier upheld the answer |
| `answered` | A later published review settled the finding as `answered`, and no verifier upheld it: none ran, or it did not finish |
| `open_at_merge`, `open_at_close` | The pull request was merged, or closed unmerged, while the finding was still `open` |

Only an `open` row changes, and only when the later review is published: a
review that waits for an approval settles once the approval publishes it, and
a wait that expires or is superseded changes nothing.

## When the pull request ends

A pull request with `open` rows that left the review search (its review was
published, or it was merged or closed) gets one state read in a discovery pass,
at most once per `schedule.outcome_check_interval_seconds` (see
[configuration](../../configuration.md#schedule)). A repository whose search
failed in that pass is not read. If the pull request was merged or closed, its
`open` rows become `open_at_merge` or `open_at_close` at the head it ended on,
other rows keep their outcome, and the store records how it ended. A pull
request with no `open` row is not read again. A pull request that is still
open, that GitHub does not return, or whose read fails changes nothing: it is
read again after the interval (a failed read logs `pull request state of
finding outcomes unreadable`). A pass reads at most the pull requests with
`open` rows that left the search; the first pass after an upgrade reads all of
them.

## When recording fails

The rows of a review and what it settles about earlier findings are written
in one transaction after GitHub accepted the review. If the store fails, the
engine logs `finding outcomes not recorded` with the `error_code`, nothing of
that review is recorded, and the review completes as published. A review that
is resumed after a restart writes its rows again; rows already stored stay,
and rows it already settled are no longer `open`. One rare case is not a
no-op: a review of the same head that publishes findings again under `P` IDs an
earlier review of that head used can move or clear tracked keys along that
chain of `P` keys when it is resumed, so a later verdict can settle the wrong
row. Outcomes never feed a review decision, so this affects only the record.
A finding published again under a key that another open row still has, and
that the review neither settles nor moves, loses its tracked key instead, so a
later verdict on that key does not also settle the moved finding. If the store
fails while a pull request ends, the engine logs `finding outcome check
failed`, and the next check after the interval ends it.

## Retention

A row stays for `store.outcome_retention_seconds` after it last changed (see
[configuration](../../configuration.md#store)). The rows do not depend on the
review: pruning a completed review after `store.review_retention_seconds`
keeps its outcomes. The state read of a pull request is kept for the same
time after it was made. See
[state and recovery](state-and-recovery.md#retention).
