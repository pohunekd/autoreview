---
kind: fixed
---
A Codex review no longer ends early when a quoted adapter exit line in the Codex transcript happens to end one read chunk of the session output: the engine decides that a reviewer has finished only on the output read up to its current end, for Claude and Codex alike. Such a review used to fail its run with `review_result_invalid` and start a fresh one.
