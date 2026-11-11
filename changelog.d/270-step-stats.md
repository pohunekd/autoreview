---
kind: added
---
The statistics document of `autoreview stats --json` and the MCP tool `review_stats` counts the runs of each review step (`history.steps`: discussion, reviewers, verifier and publication) with their outcomes, retries and median and 90th percentile durations. `autoreview serve` records the step runs in the store from this version on, so reviews it ran before count in no step.
