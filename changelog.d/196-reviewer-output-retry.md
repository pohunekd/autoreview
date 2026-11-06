---
kind: fixed
---
A reviewer output that breaks one of the engine checks (findings list, pull request and head, summary length, finding and test counts, a finding's id, side, line, path, title, body, confidence or severity, or a test's text) gets a fresh reviewer run up to the run limit instead of failing the review as `artifact_invalid`, and the log names the failed check.
