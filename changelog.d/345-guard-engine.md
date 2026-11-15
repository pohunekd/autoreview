---
kind: added
---
A policy rule with `guard` now runs the prompt-injection guard once per review, before the reviewers start. It reads the configured texts of the pull request with extra GitHub read calls (only for such a rule), up to `guards.text.max_input_bytes`, and stores the result with the review. A suspect pull request (its score reaches `threshold`, its text reached the byte bound, or `guards.text` is missing) sends one `guard_suspect` message at level `action` with signal codes only. With `on_suspect: await` a clean review that the rule would approve waits for a human approval; with `comment` a request for changes on an untrusted author becomes a comment; `log` changes nothing else. A review without a guard is unchanged.
