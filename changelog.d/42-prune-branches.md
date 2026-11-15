---
kind: added
---
`autoreview prune-branches --git-binary <PATH>` lists the review branches under `session.branch_prefix` that no review needs on the review hosts whose repositories are on this machine, and deletes them with `--yes`. It keeps every branch of a queued, running or awaiting review, of a review whose session cleanup is pending, of a pohunek session and of a worktree, and every symbolic ref, and prints each branch with its commit. `--host` picks one host, `--older-than-days` keeps recently finished reviews, and a failed deletion or a skipped `--host` host exits with status 1 (`branch_prune_incomplete`).
