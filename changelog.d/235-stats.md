---
kind: added
---
`autoreview stats` prints review statistics from the store: reviews queued, running and awaiting approval per repository and review host and, with `--since`, daily counts over UTC days (started, published, approved without a human, changes requested, superseded, failed, and the median and p90 time to publication) per repository, rule and reviewer; `--json` prints the versioned document that the new read-scope MCP tool `review_stats` also returns.
