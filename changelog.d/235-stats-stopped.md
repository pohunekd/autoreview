---
kind: added
---
The statistics document of `autoreview stats --json` and the MCP tool `review_stats` counts the stopped pull requests, whose attempts are used up or whose failure is not retried (`current.stopped`, per repository), and the failed attempts of the range per error code (`history.failed_by_code`).
