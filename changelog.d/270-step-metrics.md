---
kind: added
---
The Prometheus endpoint of `autoreview serve` exports the runs of the review steps (`discussion`, `reviewers`, `verifier`, `publication`): `autoreview_review_steps_total` per step and outcome, `autoreview_review_step_retries_total` per step, and the histogram `autoreview_review_step_seconds` of the succeeded runs per step, on the numbers of `history.steps` in `autoreview stats --json`.
