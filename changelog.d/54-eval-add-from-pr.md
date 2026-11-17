---
kind: added
---
`autoreview eval add --from-pr OWNER/NAME#N --fix SHA` writes an eval case for a defect that a review let through, from the commit that fixed it: the expected lines are the ones the fix changed in the file as the reviewed head has it, one finding per file that has an added line in the reviewed diff, and the command prints the case and the `git` commands for a fixture branch when the pull request ref moved on.
