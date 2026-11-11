---
kind: fixed
---
The next review of a pull request retargeted to another base branch is full instead of incremental, so a rule with `on_clean: approve` no longer approves commits that joined the diff only through the retarget. The log reason is `base_branch_changed`. Reviews published before this release record no base branch, so the check starts with the next published review of each pull request.
