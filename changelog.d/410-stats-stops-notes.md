---
kind: added
---
`autoreview stats` and the MCP tool `review_stats` show per day and in total the notes handed to the author (`notes_to_author`) and the pull requests stopped after their retries (`stopped`), also for days whose reviews are already pruned. Both are new counters of the history; `schema_version` stays 1 and no other field changes.
