---
kind: added
---
The new command `autoreview findings list --repo OWNER/REPO --since DATE` prints the recorded outcome of every finding of a repository published since midnight UTC of that day, oldest first, as one JSON object per line with the finding's key, reviewer, rule, severity, outcome, head SHAs and how its pull request ended.
