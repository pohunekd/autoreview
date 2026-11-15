---
kind: added
---
`autoreview stats` and the MCP tool `review_stats` keep the full daily history after the review retention: pruning moves the counts of the deleted reviews into a daily rollup in the store, which also records the pull requests stopped and the notes handed off per day. The store schema version stays 1, so an earlier release still opens the database.
