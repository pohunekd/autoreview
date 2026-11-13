---
kind: fixed
---
With several credentials, a broader credential's searches (discovery and the review queue) leave out the repositories of the credentials listed before it, so a large narrower scope no longer makes the broader credential fail with `github_search_limit_exceeded` or report a truncated queue. A single credential searches exactly as before.
