---
kind: added
breaking: true
---
The engine now records one row per published finding in the new store table `finding_outcomes`, with its reviewer, pipeline, rule, severity, confidence, head SHA and outcome (`open`, or `refuted` or `not_blocking` when the verifier demoted it); the record never changes a review or writes to GitHub. Every `config.yaml` now needs the key `store.outcome_retention_seconds` (for `scripts/env-to-config`, `HERMES_PR_REVIEW_OUTCOME_RETENTION_SECONDS`), the time a row stays after its last change: add it in the same window as the new binary, since the engine refuses to start without it, and remove it again on a rollback, since earlier releases refuse it as unknown and open the store unchanged.
