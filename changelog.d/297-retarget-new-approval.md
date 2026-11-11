---
kind: fixed
---
A clean full review after a retarget without a new commit now posts its own GitHub review instead of reusing the review published for the old base branch, so a rule with `on_clean: approve` leaves an approval that branch protection has not dismissed. A review that GitHub reports as dismissed, or one published by an earlier release, is not reused for the same head either.
