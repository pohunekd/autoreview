---
kind: changed
---
When the Codex program exits without a result, for example on a model the account does not support, the adapter passes a bounded, neutralized reason (the last `ERROR:` or `error:` line of Codex's stderr) to the engine, which logs it with `review_result_invalid` in the `error` field of the reviewer output warnings. Run `autoreview profiles install` on the review host to write the new adapter; the recorded failure and the notifications keep only the code.
