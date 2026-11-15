---
kind: security
breaking: true
---
A Codex review can no longer publish a result that the Codex transcript forged, for example a result frame and exit line that the model printed while it paused: the engine sends a fresh nonce with every Codex run, the adapter keeps it from the model and writes it into its markers, and the engine reads only markers with that nonce. Run `autoreview profiles install` for every Codex profile before you update the engine: an adapter from an earlier release refuses every Codex request before Codex runs, and the review fails with the new code `review_nonce_mismatch`. Claude profiles need nothing.
