---
kind: security
breaking: true
---
A `checkout: head` review now starts its pohunek session with `--no-repo-hooks`, so a pull request's own pohunek and git hooks never run on the review host, and it is refused before any session starts on a host without that switch (pohunek older than 0.43.1, `review_host_repo_hooks_unsupported`). The new required key `pohunek.head_checkout_host_hooks` (`refuse` or `allow`) decides whether such a review may run on a host whose own hook scripts would run in the worktree (`review_host_hooks_refused`); add it to an existing `config.yaml` before upgrading, and `scripts/env-to-config` reads it from the new required variable `HERMES_PR_REVIEW_HEAD_CHECKOUT_HOST_HOOKS`.
