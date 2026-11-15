---
kind: added
breaking: true
---
A pipeline with `intent_check: issues` hands the reviewers the issues that the pull request closes in its own repository and reports a clear mismatch with them as a blocking finding; issues of other repositories are never read, and the published review states what was compared. Every `config.yaml` now needs `limits.max_issue_characters` (for `scripts/env-to-config`, `HERMES_PR_REVIEW_MAX_ISSUE_CHARACTERS`): add it in the same window as the new binary, since the engine refuses to start without it and `autoreview validate` names it. A rollback to an earlier release must remove the key again, because earlier releases refuse it as unknown.
