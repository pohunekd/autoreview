---
kind: fixed
---
A `codex-envelope` pipeline reviews the whole pull request again after a rebase, a force push
or a merge of the base branch, as a `claude-structured` pipeline does, so a rule with
`on_clean: approve` no longer approves a rewritten history on an incremental review.
