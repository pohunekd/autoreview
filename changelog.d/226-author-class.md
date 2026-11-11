---
kind: changed
---
`autoreview candidates` classifies each author as a discovery pass does: it shows the `author_class` and the rule each pull request would run under, and lists under `skipped` the pull requests that `untrusted: skip` leaves out. `autoreview review`, `autoreview reviews` and the MCP review tools show the `author_class` of each review, and `review` and the review queue report a skipped pull request as skipped (`untrusted_author`) instead of not started.
