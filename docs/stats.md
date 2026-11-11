---
type: Reference
id: reference/stats
title: Review statistics
description: The document that autoreview stats --json prints and the MCP tool review_stats returns - the current state of the reviews, daily counts over a range of UTC days, breakdowns per repository, rule and reviewer, how each count is defined, and a jq query for each part.
source_kind: manual
intents: [debug, help]
---

# Review statistics

> **Status: after 0.3.0.** `autoreview stats` and the MCP tool `review_stats` come after
> 0.3.0. A Prometheus endpoint on the same numbers is planned in #235.

autoreview ships no dashboard. It prints the numbers a dashboard needs as one JSON document,
so any tool that reads JSON can draw them:

```
autoreview stats [--since DATE [--until DATE]] [--json]
```

- `--json` prints the document; without it, `stats` prints a short text summary of the same
  document.
- The MCP tool `review_stats` (scope `read`) takes `since` and `until` and returns the same
  document in the `result` field of its answer; see the
  [MCP server](knowledge/guides/mcp-server.md#tools).
- The document is read from the store only. `stats` asks neither GitHub nor pohunek, and
  changes nothing.

## The range

`DATE` is a UTC day written as `YYYY-MM-DD`.

- Without `--since` the document holds the current state only, and `history` is `null`.
  The current state is what a dashboard polls most often, and it needs no range.
- `--since` starts the daily history; `--until` ends it and is today (UTC) when absent.
  Both days are included.
- `--until` without `--since`, `--until` before `--since`, and any other form of a day are
  refused with `stats_range_invalid`.
- A range of more than 366 days is refused with `stats_range_too_long`, so one call reads a
  bounded part of the store.

## The document

```json
{
  "schema_version": 1,
  "generated_at": "2026-11-11T05:47:56Z",
  "current": {
    "queued": 0, "running": 1, "awaiting": 1,
    "by_repository": [{"key": "acme/widget", "queued": 0, "running": 1, "awaiting": 1}],
    "by_host": [{"key": "review-host", "queued": 0, "running": 1, "awaiting": 1}]
  },
  "history": {
    "since": "2026-11-09",
    "until": "2026-11-10",
    "pruned_before": "2026-10-12T05:47:56Z",
    "totals": {
      "started": 6, "completed": 4, "published": 3,
      "approved_without_human": 1, "approved_after_wait": 1,
      "changes_requested": 1, "commented": 0,
      "superseded": 0, "approval_expired": 0, "failed": 1,
      "publication_median_seconds": 750, "publication_p90_seconds": 9000
    },
    "days": [{"key": "2026-11-09", "started": 2, "...": "the counters of totals"}],
    "by_repository": [{"key": "acme/widget", "...": "the counters of totals"}],
    "by_rule": [{"key": "acme/*", "...": "the counters of totals"}],
    "by_reviewer": [{"key": "opus", "...": "the counters of totals"}]
  }
}
```

The `"..."` entries stand for the twelve counters of `totals`; the real document always
writes all of them.

### Top level

- `schema_version`: `1`. A change to a field's name or meaning gets a new version.
- `generated_at`: when the document was computed, RFC 3339 in UTC.
- `current`: the reviews that are not completed.
- `history`: the counts of the range, or `null` without `--since`.

```sh
autoreview stats --json | jq '{schema_version, generated_at}'
```

### Current state

- `queued`: reviews that are dispatched and wait for a worker.
- `running`: reviews that a worker runs, including one that resumes after a restart.
- `awaiting`: clean reviews that wait for an approval decision or its deadline
  (`on_clean: await`).
- `by_repository` and `by_host`: the same three counts per repository and per review host,
  sorted by `key`. The host is the one chosen when the review was dispatched.

The store does not know whether the session of a running review still lives on its host,
and a pull request that stopped after its retries has no review in these counts until
`autoreview retry` starts one; `autoreview review` shows its schedule record.

```sh
autoreview stats --json | jq '.current.by_host[] | select(.running + .queued > 0)'
```

### History

- `since`, `until`: the range; `until` is today (UTC) when `--until` is absent.
- `pruned_before`: now minus `store.review_retention_seconds`. Completed reviews last
  updated before this instant may already be deleted (see
  [retention](knowledge/concepts/state-and-recovery.md#retention)), so the counts of
  earlier days can be too low.
- `totals`: the counters over the whole range.
- `days`: one entry per UTC day of the range, in order, every day present.
- `by_repository`, `by_rule` and `by_reviewer`: the counters over the whole range per
  repository, per rule (its `match` value) and per reviewer ID, sorted by `key`. Only
  entries with a count appear. A review counts once for each of its reviewers, so the
  reviewer entries do not add up to `totals`.

```sh
autoreview stats --since 2026-11-01 --json | jq '.history.totals'
autoreview stats --since 2026-11-01 --json \
  | jq -r '.history.days[] | [.key, .started, .published, .failed] | @tsv'
autoreview stats --since 2026-11-01 --json \
  | jq '.history.by_reviewer[] | {key, published, publication_median_seconds}'
```

### Counters

One stored review is one attempt: a retry is a new attempt, so `started` and `failed` count
attempts, not pull requests. `started` counts on the UTC day the attempt was created; every
other counter counts on the UTC day it completed, and only inside the range.

- `started`: attempts created.
- `completed`: attempts completed, whatever their result.
- `published`: completed attempts that published a review on GitHub.
- `approved_without_human`: published approvals of reviews that never waited for an
  approval.
- `approved_after_wait`: published approvals of waits that `autoreview approve` or the MCP
  tool `approve` decided. The store does not record whether a person or an agent called it.
- `changes_requested`: published reviews that requested changes.
- `commented`: published reviews that commented, including a notes hand-off.
- `superseded`: attempts that a newer state of the pull request replaced: a new head, a
  closed or draft pull request, a removed label, a changed or withdrawn review request, or a
  wait that a newer review replaced.
- `approval_expired`: approval waits that reached `approval.timeout_hours` without a
  publication.
- `failed`: attempts that failed for any other reason. A failure that is retried counts
  again with its next attempt.
- `publication_median_seconds`, `publication_p90_seconds`: the median and the 90th
  percentile of the seconds from creation to completion of the published attempts, by
  nearest rank (the `ceil(p × n / 100)`-th smallest); `null` without a published attempt.
  The time of an attempt that waited for an approval includes the wait. An attempt whose
  completion is earlier than its creation (the clock went back) is left out.

`published` is the sum of the four decision counters, except for a decision this version
does not know, which an older engine may have stored. `completed` is `published`,
`superseded`, `approval_expired` and `failed` together, except for such an outcome.

```sh
autoreview stats --since 2026-11-01 --json \
  | jq '.history.totals | {published, approved_without_human, share:
        (if .published > 0 then .approved_without_human / .published else null end)}'
```

## Not in the document

The store does not keep these, so the document leaves them out:

- notes handed off per day, and pull requests stopped after their retries per day;
- the duration of each reviewer run and of the verifier, and retries per step;
- token or cost data, which pohunek does not report to autoreview;
- days before `pruned_before`, which need a rollup table that outlives the retention.

## Text form

`autoreview stats` without `--json` prints the totals of the same document:

```
Current: 0 queued, 1 running, 1 awaiting approval
  acme/widget: 0 queued, 1 running, 1 awaiting approval
  host review-host: 0 queued, 1 running, 1 awaiting approval
History 2026-11-09 to 2026-11-10 (UTC days):
  started 6, completed 4, published 3, superseded 0, approval expired 0, failed 1
  published: approved without a human 1, approved after a wait 1, changes requested 1, commented 0
  time to publication: median 750 s, p90 9000 s
  completed reviews last updated before 2026-10-12T05:47:56Z may be pruned
```

Scripts read the JSON; the text may change between versions.
