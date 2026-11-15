---
kind: added
---
`autoreview guard scan` runs the prompt-injection guard offline over a JSONL corpus of pull request texts, with the bound and the scan of the review step, and prints each pull request's score, signal codes and suspect flag and a summary (flagged share, hidden-text and truncated counts, pull requests per signal). `scripts/guard-corpus` fetches such a corpus of closed pull requests through `gh api`, `examples/guard-lexicon.txt` is a starter lexicon, and `testdata/guard/injection-corpus.jsonl` a synthetic injection corpus. A new guide, "Enable the text guard", describes shadow mode, the measurement, the targets and when to move a rule from `log` to `await` or `comment`.
