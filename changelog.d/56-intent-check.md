---
kind: added
breaking: true
---
A pipeline with `intent_check: issues` hands the reviewers the GitHub issues that the pull request closes, bounded by the new required configuration key `limits.max_issue_characters`, and reports a clear mismatch with them as a blocking finding. The published review states which issues were compared, that none is linked, or that they could not be read; a pipeline without the key is unchanged.
