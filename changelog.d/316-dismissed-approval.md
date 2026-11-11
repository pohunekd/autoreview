---
kind: fixed
---
A pull request retargeted back to a base branch whose approval GitHub dismissed at the retarget (the branch protection setting for stale approvals) gets a full review of that base branch again, so a rule with `on_clean: approve` approves it again instead of leaving it without an autoreview approval until the next push. The return looks the approval up on GitHub; an approval that still stands is not reviewed or looked up again. The log reason is `approval_dismissed`, and `autoreview retry` can retry the stopped review that replaces a dismissed approval.
