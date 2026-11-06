---
kind: changed
---
`config.yaml` accepts one Matrix backend per notification level and one for the review queue, so each level posts to its own room as the Go engine does. These backends must share `homeserver_url`, `access_token_env` and `mention_user`; a backend that differs is refused with the route key that names it.
