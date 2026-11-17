---
kind: added
---
With the optional key `schedule.signal_check_interval_seconds`, autoreview records two more signals about its published findings: an open finding of a review that requested changes becomes `dismissed` when GitHub shows that review dismissed, and the reactions of the pull request author to a finding's inline comment are stored and listed as `author_reactions` by `autoreview findings list`. It stores the GitHub ID of each published review for this, reads each pull request with open findings at most once per interval with one GraphQL query, and never writes to GitHub or changes a review. Without the key nothing is read or stored.
