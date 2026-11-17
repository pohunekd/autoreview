---
kind: changed
breaking: true
---
Every pipeline of `policy.yaml` needs `repository_review_rules: read` or `ignore`, and `config.yaml` needs `limits.max_review_rules_bytes`; with `read` the reviewers and the verifier get the repository's `REVIEW.md` of the base commit, and its `skip` globs turn findings in those paths into notes. Add `repository_review_rules: ignore` to keep today's behaviour; an older release refuses both keys, so remove them to roll back.
