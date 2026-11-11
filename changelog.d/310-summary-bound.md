---
kind: fixed
---
A `claude-structured` reviewer request bounds the summary by `limits.min_summary_characters` and `limits.max_summary_characters` in its JSON schema, so Claude Code asks the reviewer to fix a summary that is too long or too short within the run, instead of the engine refusing it and starting a fresh session. The schema no longer requires 40 characters when the configured minimum is lower.
