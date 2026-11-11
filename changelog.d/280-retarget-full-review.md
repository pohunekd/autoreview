---
kind: fixed
---
A pull request retargeted to another base branch without a new commit now gets a full review of the same head, so the commits that joined the diff through the retarget are reviewed. That review settles the blocking findings of the published review. An approval published before the retarget is not dismissed by autoreview; branch protection that dismisses stale approvals removes it when the retarget brings new changes.
