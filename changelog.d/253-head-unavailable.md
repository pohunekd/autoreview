---
kind: fixed
breaking: true
---
A review on a `checkout: base` pipeline whose pull request head commit is not in the review host's repository stops with the new code `review_head_unavailable` and publishes nothing, for Claude and Codex reviewers, instead of a Claude review that could not read the change; a verifier refused that way publishes the claims unverified, as after a verifier timeout. Neither the reviewer run nor the scheduler retries the stop; fetch the head on the review host and retry the pull request. The Claude adapter now checks the review checkout as the Codex adapter does and names that checkout in its result, and the engine refuses a Claude result without it as `review_adapter_outdated`, so run `autoreview profiles install` for every Claude and Codex profile on each review host in the same window as the upgrade.
