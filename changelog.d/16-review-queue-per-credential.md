---
kind: changed
---
The review queue lists only the review requests in repositories that the credentials' `match` covers. A credential limited to owners or repositories no longer lists requests elsewhere; with `match: ["*"]` the queue is unchanged.
