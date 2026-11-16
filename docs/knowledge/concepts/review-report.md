---
type: Concept
id: concept/review-report
title: Review report
description: What autoreview report measures over a range of UTC days - the time from the creation of a review attempt to its publication, the share of decisions approved without a human, waiting for a human or requesting changes, and the outcomes of the published findings per reviewer, severity and repository - how each number is computed from the store, how to read it, and what it does not measure yet.
source_kind: manual
intents: [help, debug]
---

# Review report

> **Status: after 0.3.0.** `autoreview report` comes after 0.3.0. The weekly line
> in the review queue digest, the reviewer disagreement rate and the verifier
> demotion rate are not available yet (#52).

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
  queue and an approval wait. It starts at the attempt, not at the review
  request, so a retried review counts from the attempt that published; the time
  from the review request comes with a later change (#52).

A share is rounded half up to one decimal; with nothing decided every share is
`null` in the JSON and `none` in the text, and so is a time without a published
attempt.

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

## Filters and retention

The days are read as `autoreview stats` reads them, and a malformed or too long
range gets the same codes, `stats_range_invalid` and `stats_range_too_long`,
whose messages name `stats`.

- `--repo` counts one repository. The review part then comes from the
  repository's own counts of `autoreview stats`, which cover pruned reviews too.
- `--pipeline` counts the reviews and findings of one pipeline of the policy.
  Pruned reviews are kept only as daily counts without their pipeline, so with
  `--pipeline` the range must start after now minus
  `store.review_retention_seconds`; an earlier start is refused with
  `report_range_pruned`.
- Finding outcomes are deleted after `store.outcome_retention_seconds`, so a
  range that starts before now minus that retention is refused with
  `report_range_pruned` as well.
- Both checks use the retention that is configured now. Days that an earlier
  prune with a shorter retention already cut, or that a clock set back makes
  look newer, are not refused and can count too low. Without `--pipeline`, the
  review part reads the daily counts of `autoreview stats`, which miss reviews
  pruned by an engine before that rollup existed or by an earlier release after
  a rollback (see [`autoreview stats`](../../stats.md)); those days can count
  too low as well.

## Not measured yet

These numbers need data that the report does not read yet (#52):

- **Time from the review request**: the store keeps when an attempt was created,
  not when the review was requested.
- **Verifier demotion rate**: the outcome records keep the findings the verifier
  demoted, but not the blocking claims it confirmed. The verifier's own verdicts
  are kept with the review only until it is pruned, so the report gives demoted
  counts and no rate.
- **Reviewer disagreement**: in the outcome records a merged finding keeps only
  its first reviewer, and a reviewer without findings has no row. Each
  reviewer's own result is kept with the review only until it is pruned.
