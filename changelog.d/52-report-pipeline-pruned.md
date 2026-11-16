---
kind: changed
---
`autoreview report --pipeline` now also covers pruned reviews: the prune keeps the report's counts per repository and pipeline, so a `--pipeline` range is no longer refused because it starts before the review retention. Besides the outcome-retention check, it refuses with `report_range_pruned` a range with a day that 0.4.0 or 0.5.0 pruned, because those days have daily counts but none per pipeline; reviews pruned by a release without the rollup count as zero. `stats` output is unchanged.
