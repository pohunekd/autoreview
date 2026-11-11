---
kind: fixed
---
`autoreview retry` and the MCP `retry` tool start a new attempt of a stopped review of a pull request retargeted to another base branch; a review of the same head against the earlier base branch no longer refuses it with `review_already_completed`. A pull request retargeted away and back before its retry is due no longer notifies the same failure twice or restarts its automatic retries.
