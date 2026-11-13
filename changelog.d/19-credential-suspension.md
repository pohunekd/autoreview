---
kind: added
---
A credential whose token GitHub refuses while `serve` runs is suspended: the new `credential_invalid` event (optional in the policy, default `action`) is raised once, its repositories are not searched and its queued reviews wait, while the other credentials keep reviewing. A search that GitHub refuses checks that credential's token right after the pass, and after every pass the token of each suspended credential is checked again; a check that GitHub accepts lifts the suspension. A GitHub call answered with HTTP 401 now fails as `credential_invalid` instead of `github_request_failed`: the step is retried within `schedule.step_retry_seconds`, then the review stops and is not retried automatically.
