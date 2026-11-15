---
kind: fixed
---
A Codex review no longer ends early when the Codex transcript quotes an adapter exit line, for example in a pull request diff or a file the model read: the engine waits until the exit line is the last line of the session output. Such a review used to fail its run with `review_result_invalid` and start a fresh one; no adapter reinstall is needed.
