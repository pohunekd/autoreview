---
kind: added
breaking: true
---
A recorded finding that is still open when its pull request is merged or closed now gets the outcome `open_at_merge` or `open_at_close` at the head the pull request ended on; the engine finds this with one state read per pull request that left the review search, at most once per `schedule.outcome_check_interval_seconds`. Every `config.yaml` now needs that key (for `scripts/env-to-config`, `HERMES_PR_REVIEW_OUTCOME_CHECK_INTERVAL_SECONDS`): add it in the same window as the new binary, since the engine refuses to start without it, and remove it again on a rollback, since earlier releases refuse it as unknown and open the store unchanged.
