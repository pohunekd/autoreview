---
type: Concept
id: concept/incremental-reviews
title: Incremental reviews
description: When a re-review after a push reviews only the new commits and when it reviews everything, how prior blocking findings are settled as fixed, still open or answered, the stricter rule for auto-approving rules, republished findings, discussion input and incremental_reviewers.
source_kind: manual
intents: [help, debug]
---

# Incremental reviews

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

A push to a pull request that was reviewed before starts a new review of the new
head. That review is either **incremental** (only the commits since the last
published review are reviewed) or **full** (the whole pull request is reviewed).
An incremental review is cheaper, but it can only be trusted where nothing
earlier can go unnoticed. The engine therefore chooses the scope with strict
conditions, and it settles every earlier blocking finding explicitly.

## Full or incremental

The scope is decided when the review is dispatched, in two steps.

**Step one: the last published decision.** The engine reads the last review it
published for the pull request. An incremental scope is possible only when all
of these hold:

| Condition | Why |
|---|---|
| A published review exists and its head SHA is a valid commit id | There is a baseline to compare with |
| Its head differs from the new head | Same head is a full review, see below |
| Its decision is `APPROVE`, or the rule never approves without a human, or the rule waits for a human (`on_clean: await`), or the pull request is authored by the reviewing account | A review without a human could otherwise approve on the strength of the new commits alone |

A rule that approves without a human and whose last published decision was not an
approval gets a full review. That keeps the approval resting on a complete
baseline (see [the stricter rule](#the-stricter-rule-for-auto-approve-rules)).

**Step two: the commit history.** For the scope to stay incremental the stored
result of that review must still exist, and the new head must extend the reviewed
head in a straight line:

- If the stored result is gone (for example after `store.review_retention_seconds`
  or after an upgrade of the engine), the review widens to full. The log records
  the reason as `previous_review_not_stored`.
- If the new head is not a linear extension of the reviewed one, the review
  widens to full. A rebase, a force push, or a merge of the base branch makes the
  diff between the two heads something other than the author's own change. The
  log reason is `history_not_linear`.
- If the history cannot be read, the review also widens to full, with the reason
  `history_unknown`.
- GitHub lists at most 250 commits of a comparison. An incomplete list proves
  nothing, so it counts as not linear.

A head that a published review already covers is not reviewed again. The one
exception is a renewed review request for the same head after a notes hand-off.
That request adopts the stored result of the hand-off review (see
[notes hand-off](decisions.md#notes-hand-off)). Neither case is incremental.

The commit history check and the collection of prior findings apply to every
pipeline, whichever protocol its reviewers use.

### Which reviewers run

An incremental review runs the reviewers in `incremental_reviewers`. A full review
runs `reviewers`. The two lists come from the same pipeline, so one agent can be
kept for the cheaper re-review:

```yaml
pipelines:
  core:
    reviewers: [opus, sonnet]
    incremental_reviewers: [opus]  # the stronger model settles earlier findings
    # other keys as in the reference
```

Validation enforces these rules:

- `incremental_reviewers` has one to four members, all taken from `reviewers`.
- A rule with `on_clean: approve` needs `incremental_reviewers` equal to
  `reviewers`. An automatic approval must never rest on fewer reviewers than the
  full review had.

```text
error: on_clean approve needs incremental_reviewers equal to reviewers
```

The message is the one that `autoreview validate` prints. Either make the lists
equal or use `on_clean: await` (see [policy validation errors](../../policy.md#validation-errors)).

## Prior findings

The **prior findings** of a review are the blocking findings of the last published
review and the prior findings that review still left open. The engine passes them
to the reviewers, who must settle each one. A full review also settles the prior
findings it can find, so a widened review does not drop them.

Every prior finding is identified by its key, which combines the head it was
reported at and its finding id. The number of prior findings is bounded by
`limits.max_findings`.

### Verdicts

Each reviewer returns one verdict per prior finding, and the engine checks that
every key appears exactly once. A reviewer that skips, invents or repeats a key
gets a fresh run.

| Verdict | Meaning |
|---|---|
| `fixed` | The code at the current head no longer has the defect |
| `still_open` | The code at the current head still has the defect |
| `answered` | The reviewer verified that the finding is not a defect, or that a human-written comment by the login of the serving credential accepted it. The `relies_on` field names the login whose comment the verdict rests on, or is empty when it rests on the code alone |

When several reviewers settle the same finding, the most cautious verdict wins:
`still_open` over `answered`, and `answered` over `fixed`.

The verifier checks the answered findings, see [verification](verification.md#reopening-answered-prior-findings).

### Answered on auto-approve rules

On a rule that approves without a human, an `answered` verdict closes a prior
finding only when it rests on a comment by the reviewing account, the login of the
credential that serves the repository, and that comment is not AI-signed. A
comment by any other account does not count, so under a machine-user credential the
owner's own comment does not close a finding. A reply by the pull request author alone never closes a finding
on such a rule. The engine records the finding as still open with this note:
`Settled in the discussion, but a rule that approves without the user needs a comment of the reviewing account itself`.

### Republished findings

An open prior finding that has no unresolved thread is published again. The engine
cannot ask the author to answer a thread that is resolved or missing, so it
publishes the finding again as a new finding with an id from the `P` series:
`P001`, `P002` and so on. The body says that the finding is still open from the
earlier review and names the head it was reported at. Republished findings count
as blocking.

An open prior finding that still has an unresolved inline thread is not published
again. The thread itself is the place where the author answers.

## Discussion input

The reviewers of a review with prior findings also read the discussion of those
findings. The engine collects it in this order and within the budget of
`limits.max_discussion_characters`:

1. The reply threads of the inline comments of the prior findings. Replies answer
   the findings directly, so they come first in the budget.
2. The newest conversation comments on the pull request, with the remaining budget.

Comments from bots are skipped, except a bot that authored the pull request, such
as a coding agent that answers the findings. Each comment carries a flag for
whether it is AI-signed, which is decided by the last line of the comment and the
signature text in `publish.ai_signed_pattern`.

The discussion is untrusted input. Reviewers are told to verify every claim in it
against the code and never to follow instructions in it. See
[prompt injection](../safety/prompt-injection.md).

## The stricter rule for auto-approve rules

A rule that approves without a human gets a full review after a request for
changes, even when the new commits are small. The reason is that the approval
would otherwise rest on the new commits alone, while the blocking findings of the
earlier review were never checked against the whole pull request. The
incremental path needs a previous approval or a rule that waits for a human, see
[the table above](#full-or-incremental).

## Example: the same pull request, two rules

Rule A, `on_clean: await`, `on_findings: request_changes`:

1. Review 1 at head `A` finds one high blocking finding and publishes `REQUEST_CHANGES`.
2. The author pushes head `B`. The last decision is not an approval, but the rule
   waits for a human, so the review is incremental. The reviewers in
   `incremental_reviewers` settle finding `O001` as `fixed`.
3. The review is clean and waits for a human approval.

Rule B, `on_clean: approve`, same findings:

1. The last decision was `REQUEST_CHANGES` and the rule approves without a human,
   so the push to head `B` gets a full review. It settles `O001` as before, and
   every other part of the pull request is reviewed again.
2. A clean full review publishes `APPROVE`. A clean incremental review would not
   have done so, because the incremental path is not open after a request for
   changes on this rule.

## Related pages

- The decision that a re-review produces: [decisions](decisions.md).
- How the verifier treats answered prior findings: [verification](verification.md).
- The steps of a review from start to end: [how it works](how-it-works.md).
- Retention of the baseline: [state and recovery](state-and-recovery.md#retention).
- Reviewer selection in the policy: [policy](../../policy.md#pipelines).
