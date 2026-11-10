---
kind: fixed
---
Codex reviewers work on pipelines with `checkout: base`, which the examples use and untrusted authors require: the Codex adapter checks the base checkout locally and Codex reads the change through git. The pull request head must already be in the review host's repository; otherwise the adapter refuses the run with a clear message.
