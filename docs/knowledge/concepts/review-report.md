---
type: Concept
id: concept/review-report
title: Review report
description: What autoreview report measures over a range of UTC days - the time from the creation of a review attempt and from the review request to its first publication, the share of decisions approved without a human, waiting for a human or requesting changes, the reviewer disagreement rate, the verifier demotion rate per reviewer, and the outcomes of the published findings per reviewer, severity and repository - how each number is computed from the store and how to read it.
source_kind: manual
intents: [help, debug]
---

# Review report

> **Status: after 0.4.0.** `autoreview report` and its weekly line in the review
> queue digest come after 0.4.0.

`autoreview report` gives a periodic, comparable picture of how the reviews
perform, so a change of a prompt, a model or a policy can be judged on numbers.
It reads the store only: it asks neither GitHub nor pohunek and changes nothing.
The command and its options are in the [CLI reference](../../cli.md#autoreview-report).

## What it counts

The report has two parts, and each counts different things:

- **Reviews** are the review attempts completed in the range, counted as
  [`autoreview stats`](../../stats.md#counters) counts them: one attempt per
  stored review, on the UTC day it completed. A retry is a new attempt.
- **Findings** are the findings published in the range, each with the outcome it
  has now, as [finding outcomes](finding-outcomes.md) records it. A finding
  published on the last day of the range and fixed a week later counts as
  `fixed` in a report made after the fix.

## Review metrics

- **Decided**: published attempts plus approval waits that reached their
  deadline (`approval_expired`). This is the whole of every share below.
- **Approved without a human**: approvals published without any wait, in
  percent of decided: the reviews that needed no person at all.
- **Waited for a human**: approvals after a wait plus waits that expired, in
  percent of decided. These reviews cost a person's attention.
- **Changes requested** and **commented**: in percent of decided.
- **Attempt creation to publication**
  (`attempt_to_publication_median_seconds`, `attempt_to_publication_p90_seconds`):
  the median and the 90th percentile, by nearest rank, of the seconds from the
  creation of a published attempt to its completion. It includes the time in the
  queue and an approval wait. It starts at the attempt, so a retried review
  counts from the attempt that published.
- **Review request to publication**
  (`request_to_publication_median_seconds`, `request_to_publication_p90_seconds`):
  the same percentiles of the seconds from the review request to the completion
  of its first published attempt: the wait of the person who asked. The request
  time is the newest `review_requested` event of the reviewing login when
  discovery read the pull request. Each request counts once, at the attempt that
  published first for it (the earliest completion of the same repository, pull
  request and request event); a retry that published or a new head under the
  same request, not renewed, adds nothing. A label trigger has no request, and a
  review stored before this field existed has no request time; such attempts
  are left out, not counted as zero. A request whose first publication has no
  request time, or one after the completion (the clocks of GitHub and the host
  differ), is left out as well. With no attempt left, the time is `none`.

A share is rounded half up to one decimal; with nothing decided every share is
`null` in the JSON and `none` in the text, and so is a time without a published
attempt.

## Verifier demotion rate

`reviews.by_reviewer` gives, per reviewer ID, how often the
[verifier](verification.md) turned that reviewer's blocking claims into notes:

- `verifier_checked`: the blocking findings that a verification gave a verdict,
  in every attempt completed in the range, published or not, on the UTC day it
  completed. Each attempt runs the verifier at most once; a resumed attempt
  reuses its stored verdicts, and an attempt that adopts a hand-off runs no
  verifier, so nothing counts twice.
- `verifier_demoted`: those found `not_blocking` or `refuted`, and
  `verifier_demoted_percent`: in percent of `verifier_checked`, rounded as the
  shares above, `null` (`none`) with nothing checked.

A verification that did not finish checks nothing and counts nowhere. The
reviewer is the one the reviewers' combined findings name; a merged finding
counts for its first reviewer, as in the finding metrics. A high rate means the
reviewer's blocking claims often do not hold, so compare it per reviewer over
the same range. It differs from the `demoted` column of the findings table,
which counts the published findings of the range by the day they were
published, and has no whole.

## Reviewer disagreement

The disagreement compares the reviewers of one attempt with each other, from
each reviewer's own result before the results are combined and before the
verifier checks them:

- `multi_reviewer`: the published attempts of the range, on the UTC day they
  completed, in which two or more reviewers have a stored result. An attempt
  with one reviewer, such as an incremental review that runs one, compares
  nothing. An attempt that adopts a hand-off runs no reviewer; the attempt that
  handed the notes over counts instead. A resumed attempt keeps one result per
  reviewer, so nothing counts twice. Failed, superseded and expired attempts
  publish nothing and do not count.
- `disagreed`: those where at least one reviewer made a blocking claim and at
  least one made none (no finding, or notes only), and `disagreed_percent`: in
  percent of `multi_reviewer`, rounded as the shares above, `null` (`none`)
  without such an attempt.
- `blocked_alone` in each `reviews.by_reviewer` entry: the disagreeing attempts
  in which that reviewer made a blocking claim. With three reviewers, two that
  block against one that does not both count.

A claim counts as the reviewer made it, so an attempt whose lone blocking claim
the verifier later demoted still disagreed. A high rate means the reviewers
often judge the same change differently; compare `blocked_alone` with the
reviewer's `verifier_demoted_percent` to see whether its lone claims hold.

## Finding metrics

Each part, the total and every reviewer, severity and repository, counts:

- `findings`: findings published, and `blocking`: those that held the merge back
  when they were published.
- One count per outcome: `open`, `fixed`, `answered`, `refuted`,
  `not_blocking`, `open_at_merge` and `open_at_close`. The text table shows
  `refuted` and `not_blocking` together as `demoted`: blocking claims that the
  [verifier](verification.md) turned into notes.
- `fixed_percent`: `fixed` in percent of `fixed` plus `open_at_merge`, a proxy
  for precision. A finding that the author fixed was probably right; one that
  was still open when the pull request merged was probably not worth the
  author's time, or the author disagreed. Notes count as well, so compare
  severities rather than the total.

A merged finding counts under its first reviewer only. Read `fixed_percent`
over a range that ended some time ago: right after a range, most findings are
still `open`.

## Weekly line

With `notifications.weekly_report: digest` in the policy, the hourly
`autoreview review-queue` run appends this text, for every repository and
pipeline from 7 UTC days before today to today, to its message once a week. The
range of two consecutive reports shares one day. See
[weekly report](notifications.md#weekly-report) for when it is sent and retried.

## Filters and retention

The days are read as `autoreview stats` reads them, and a malformed or too long
range gets the same codes, `stats_range_invalid` and `stats_range_too_long`,
whose messages name `stats`.

- `--repo` counts one repository. The review part then comes from the
  repository's own counts of `autoreview stats`, which cover pruned reviews too.
- `--pipeline` counts the reviews and findings of one pipeline of the policy.
  The prune keeps the review counts, times, verifier's counts and reviewer
  disagreement of each pipeline in the same daily rollup, per repository and
  pipeline, so a pipeline's report covers pruned reviews as `--repo` does. Each
  pruned review counts once for its day and once for its pipeline. A day where
  the two differ holds reviews that a release before these counts pruned, for
  example after a rollback; a range with such a day is refused with
  `report_range_pruned`, because their pipeline is not known.
- Finding outcomes are deleted after `store.outcome_retention_seconds`, so a
  range that starts before now minus that retention is refused with
  `report_range_pruned` as well.
- That check uses the retention that is configured now. Days that an earlier
  prune with a shorter retention already cut, or that a clock set back makes
  look newer, are not refused and can count too low. The review part reads the
  daily counts of `autoreview stats`, which miss reviews pruned by an engine
  before that rollup existed or, without `--pipeline`, by such an engine after a
  rollback (see [`autoreview stats`](../../stats.md)); those days can count too
  low as well.
- The prune keeps the request times in the same daily rollup, per repository,
  and notes each request whose first publication it deleted, so a later attempt
  of that request that is still kept adds no second time. Reviews pruned before
  the request time was stored, or by an earlier release after a rollback, have
  none there, and a later attempt of such a request can count; an earlier
  release ignores these rows.
- The prune keeps the verifier's counts in the same daily rollup, per
  repository and reviewer. Reviews pruned before these counts were kept, or by
  an earlier release after a rollback, have none there, so their days count too
  few checked findings and the rate covers only the kept attempts.
- The prune keeps the reviewer disagreement in the same daily rollup, per
  repository and per repository and reviewer. Reviews pruned before these
  counts were kept, or by an earlier release after a rollback, have none there,
  so their days count too few attempts and the rate covers only the kept ones.
