---
kind: fixed
---
A reviewer output that breaks one of the engine checks (pull request and head, summary length, finding and test counts, or a finding's id, line, path, title, body, confidence or severity) gets a fresh reviewer run up to the run limit instead of failing the review as `artifact_invalid`, and the log names the failed check.
