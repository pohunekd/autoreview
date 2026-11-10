---
kind: fixed
---
A review on a `checkout: base` pipeline whose pull request head commit is not in the review host's repository stops with the new code `review_head_unavailable` and publishes nothing, for Claude and Codex reviewers and the verifier alike, instead of a Claude review that could not read the change. The Claude adapter now checks the review checkout as the Codex adapter does, and neither the reviewer run nor the scheduler retries the stop; fetch the head on the review host and retry the pull request. Review hosts need `autoreview profiles install` for every Claude and Codex profile after the upgrade.
