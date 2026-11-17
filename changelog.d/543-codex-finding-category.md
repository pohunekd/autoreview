---
kind: added
breaking: true
---
Reviewers of the `codex-envelope` kind may give each finding the optional `category` slug that `claude-structured` reviewers already can, so the `guidance` suggestions count their fixed findings too, and a `category` that is not a slug is dropped with one warning. The engine now names `autoreview-codex/v3` in its Codex requests: run `autoreview profiles install` for every Codex profile before you update the engine, because an adapter from an earlier release refuses the request before Codex runs and the review fails with `review_nonce_mismatch`, while an engine rollback needs no reinstall, as the new adapter drops the `category` for an older engine.
