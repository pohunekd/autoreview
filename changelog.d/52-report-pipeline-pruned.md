---
kind: changed
---
`autoreview report --pipeline` now also covers pruned reviews: the prune keeps the report's counts per repository and pipeline, so a `--pipeline` range is no longer refused because it starts before the review retention. It refuses with `report_range_pruned` only a range with a day that an earlier release pruned, because those days have no counts per pipeline. `stats` output is unchanged.
