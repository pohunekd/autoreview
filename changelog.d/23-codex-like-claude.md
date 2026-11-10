---
kind: changed
breaking: true
---
A `codex-envelope` reviewer reports notes that do not block and settles the earlier blocking
findings, as a `claude-structured` reviewer does, so a Codex re-review after a push covers only
the new commits. A Codex profile must run the adapter in `adapters/codex/`: the report of an
older adapter, without `blocking` and `prior_findings`, stops the review with
`review_adapter_outdated`.
