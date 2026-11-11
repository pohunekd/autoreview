---
type: Reference
id: reference/stats
title: Review statistics
description: The document that autoreview stats --json prints and the MCP tool review_stats returns - the current state of the reviews, daily counts over a range of UTC days, breakdowns per repository, rule and reviewer, how each count is defined, and a jq query for each part - and the Prometheus metrics of autoreview serve on the same numbers.
source_kind: manual
intents: [debug, help]
---

# Review statistics

> **Status: after 0.3.0.** `autoreview stats`, the MCP tool `review_stats` and the
> Prometheus endpoint of `autoreview serve` come after 0.3.0.

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
  changes nothing. Both forms read the discovery state. Without `--since` they read the
  reviews that are not completed, through the status index. With `--since` they read the
  whole `reviews` table, so the cost grows with `store.review_retention_seconds`, which
  decides how many reviews the store keeps. The reviews and the discovery state come from
  one read transaction, so all counts describe the same moment.
- `autoreview serve` can serve the same numbers to Prometheus; see
  [Prometheus metrics](#prometheus-metrics).

## The range

`DATE` is a UTC day written as `YYYY-MM-DD`.

- Without `--since` the document holds the current state only, and `history` is `null`.
  The current state is what a dashboard polls most often, and it needs no range.
- `--since` starts the daily history; `--until` ends it and is today (UTC) when absent.
  Both days are included.
- `--since` after today (UTC), `--until` after today, `--until` without `--since`,
  `--until` before `--since`, and any other form of a day are refused with
  `stats_range_invalid`.
- A range of more than 366 days is refused with `stats_range_too_long`, so one document
  holds a bounded number of days.

## The document

```json
{
  "schema_version": 1,
  "generated_at": "2026-11-11T05:47:56Z",
  "current": {
    "queued": 0, "running": 1, "awaiting": 1, "stopped": 1,
    "by_repository": [{"key": "acme/widget", "queued": 0, "running": 1, "awaiting": 1}],
    "by_host": [{"key": "review-host", "queued": 0, "running": 1, "awaiting": 1}],
    "stopped_by_repository": [{"key": "acme/widget", "stopped": 1}]
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
    "by_reviewer": [{"key": "opus", "...": "the counters of totals"}],
    "failed_by_code": [{"key": "pohunek_session_start_failed", "failed": 1}]
  }
}
```

The `"..."` entries stand for the twelve counters of `totals`; the real document always
writes all of them.

### Top level

- `schema_version`: `1`. A change to a field's name or meaning gets a new version.
- `generated_at`: when the document was computed, RFC 3339 in UTC.
- `current`: the queued and running reviews, the approval waits without a decision, and
  the stopped pull requests (attempts used up, or a failure that is not retried).
- `history`: the counts of the range, or `null` without `--since`.

```sh
autoreview stats --json | jq '{schema_version, generated_at}'
```

### Current state

- `queued`: reviews that are dispatched and wait for a worker.
- `running`: reviews that a worker runs, including one that resumes after a restart.
- `awaiting`: clean reviews that wait for an approval decision or its deadline
  (`on_clean: await`), the reviews that `autoreview awaiting` lists. A wait that is already
  decided (approved, expired or superseded) counts nowhere until `serve` continues it.
- `stopped`: pull requests whose schedule record is `failed`: their attempts are used up,
  or the failure is not retryable (see the
  [scheduler states](knowledge/concepts/state-and-recovery.md#scheduler-states)). Such a
  pull request has no review in the three counts above. It counts here from the discovery
  pass after its failed review, which decides that the failure is not retried. It leaves `stopped` when
  discovery sees a new head or a new review request (the record becomes `waiting` before a
  review starts), when `autoreview retry` or the MCP tool `retry` starts a review, and when
  discovery drops its record after the pull request left every trigger. `autoreview review`
  shows the record.
- `by_repository` and `by_host`: the queued, running and awaiting counts per repository
  and per review host, sorted by `key`. The host is the one chosen when the review was
  dispatched.
- `stopped_by_repository`: `stopped` per repository, sorted by `key`, only repositories
  with a stopped pull request. A schedule record names no review host, so `by_host` has no
  `stopped`.

`stopped` counts pull requests, the other counts count reviews (attempts). The store does
not know whether the session of a running review still lives on its host.

```sh
autoreview stats --json | jq '.current.by_host[] | select(.running + .queued > 0)'
autoreview stats --json | jq -r '.current.stopped_by_repository[] | "\(.key) \(.stopped)"'
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
- `failed_by_code`: the attempts that `failed` counts over the range, per stored error code
  (`key`), sorted by code. The entries add up to `totals.failed`; a superseding code such as
  `head_changed` counts in `superseded` and is not listed. The codes are those of the
  [CLI error codes](cli.md#error-codes) and the
  [troubleshooting](knowledge/runbooks/troubleshooting.md) table, including
  `workflow_failed` for a failure without a more specific code.

```sh
autoreview stats --since 2026-11-01 --json | jq '.history.totals'
autoreview stats --since 2026-11-01 --json \
  | jq -r '.history.days[] | [.key, .started, .published, .failed] | @tsv'
autoreview stats --since 2026-11-01 --json \
  | jq '.history.by_reviewer[] | {key, published, publication_median_seconds}'
autoreview stats --since 2026-11-01 --json \
  | jq -r '.history.failed_by_code | sort_by(-.failed)[] | "\(.failed) \(.key)"'
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
  retarget to another base branch, a closed or draft pull request, a removed label, a
  changed or withdrawn review request, or a wait that a newer review replaced.
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

- notes handed off per day, and stops per day: `stopped` is a current count, because the
  schedule record does not keep the time of the stop;
- the duration of each reviewer run and of the verifier, and retries per step;
- token or cost data, which pohunek does not report to autoreview;
- days before `pruned_before`, which need a rollup table that outlives the retention.

## Text form

`autoreview stats` without `--json` prints the totals of the same document:

```
Current: 0 queued, 1 running, 1 awaiting approval
  acme/widget: 0 queued, 1 running, 1 awaiting approval
  host review-host: 0 queued, 1 running, 1 awaiting approval
Pull requests stopped: 1
History 2026-11-09 to 2026-11-10 (UTC days):
  started 6, completed 4, published 3, superseded 0, approval expired 0, failed 1
  published: approved without a human 1, approved after a wait 1, changes requested 1, commented 0
  failed by code: pohunek_session_start_failed 1
  time to publication: median 750 s, p90 9000 s
  completed reviews last updated before 2026-10-12T05:47:56Z may be pruned
```

Scripts read the JSON; the text may change between versions.

## Prometheus metrics

With a [`metrics` section](configuration.md#metrics) in `config.yaml`, `autoreview serve`
answers `GET /metrics` on `metrics.listen` in the Prometheus text format, version 0.0.4.
Without the section `serve` opens no port and counts nothing for it. Only `serve` runs the
endpoint, and it stops with `serve`.

```yaml
scrape_configs:
  - job_name: autoreview
    static_configs:
      - targets: ["127.0.0.1:9464"]
```

- The endpoint answers only `GET /metrics` whose `Host` header is in
  `metrics.allowed_hosts`, with the check of the MCP server (400 without a valid `Host`, 403
  for another one; a request without `Host` whose target is absolute, such as
  `http://127.0.0.1:9464/metrics`, is checked by the host of its target). A request with
  an `Origin` header gets 403, another path 404 and another method 405. It has no token:
  keep `listen` on loopback and see the [threat model](knowledge/safety/threat-model.md).
- The gauges are read from the store at each scrape, with the read of `stats` without
  `--since`: the reviews that are not completed, through the status index, and the
  discovery state. A scrape never reads the completed reviews.
- The counters live in the memory of the `serve` process and start at zero with it. A
  restart reads as a counter reset, so query them with `rate()` or `increase()`. They count
  what the workers of this `serve` do; `poll`, `retry` and `mcp` add nothing until `serve`
  runs the review.
- A series appears with its first count, so a missing series means zero. The labels are
  `repository`, `host` (the review host chosen at dispatch), `rule` (the `match` value of
  the rule), `decision` and `error_code`; no label holds review text.

### Gauges

- `autoreview_reviews_queued`, `autoreview_reviews_running`, `autoreview_reviews_awaiting`
  (labels `repository`, `host`): `queued`, `running` and `awaiting` of the
  [current state](#current-state) per repository and review host. A pair without a queued,
  running or awaiting review has no sample.
- `autoreview_pull_requests_stopped` (label `repository`): `current.stopped_by_repository`,
  the pull requests stopped after their retries.

```promql
sum by (repository) (autoreview_reviews_queued + autoreview_reviews_running)
sum by (host) (autoreview_reviews_running)
sum(autoreview_pull_requests_stopped) > 0
```

### Counters

Each counter has the labels `repository` and `rule`. A completed review is classified as
the document classifies it, so each counter counts what its [counter](#counters) of the
document counts, over the life of the process instead of per UTC day.

- `autoreview_reviews_started_total`: attempts that a worker took from the queue. The
  document counts `started` when the attempt is created; this counter counts it when the
  attempt leaves the queue, so it lags while every worker is busy. A review that resumes as
  `running` after a restart and a decided wait that continues are not counted again.
- `autoreview_reviews_completed_total`: `completed`.
- `autoreview_reviews_published_total` (label `decision`): `published` per decision:
  `approved_without_human`, `approved_after_wait`, `changes_requested`, `commented`, or
  `other` for a decision this version does not know. The decisions add up to `published`.
- `autoreview_reviews_superseded_total`: `superseded`.
- `autoreview_reviews_approval_expired_total`: `approval_expired`.
- `autoreview_reviews_failed_total` (label `error_code`): `failed` per stored error code, as
  `failed_by_code`.

```promql
sum by (decision) (increase(autoreview_reviews_published_total[1d]))
sum by (error_code) (increase(autoreview_reviews_failed_total[1d]))
sum(increase(autoreview_reviews_published_total{decision="approved_without_human"}[7d]))
  / sum(increase(autoreview_reviews_published_total[7d]))
```

### Histogram

- `autoreview_review_publication_seconds` (labels `repository`, `rule`): the seconds from
  creation to completion of each publication, as `publication_median_seconds` measures
  them, including an approval wait. The buckets `le` are 60, 300, 900, 1800, 3600, 14400,
  43200, 86400 and 259200 seconds and `+Inf`. A publication whose completion is earlier
  than its creation is left out, so `_count` can be below the publications.

```promql
histogram_quantile(0.9,
  sum by (le) (rate(autoreview_review_publication_seconds_bucket[1d])))
```

The durations and retries of the workflow steps are not exported.
