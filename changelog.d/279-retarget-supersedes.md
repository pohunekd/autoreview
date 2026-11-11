---
kind: fixed
---
A review that is running when its pull request is retargeted to another base branch now publishes nothing, so a rule with `on_clean: approve` no longer approves a diff it read against the old base. The review ends as superseded with the code `base_branch_changed`, and the next review of the pull request is full.
