---
kind: changed
---
When Claude Code exits with a status other than 0, the engine logs the end of its stderr, bounded and neutralized, with `review_wrapper_invalid` in the `error` field of the reviewer output warnings; when the Codex adapter refuses what a Codex run left (no report, an invalid report or a changed checkout), it passes its message as the reason that the engine logs with `review_result_invalid`. Run `autoreview profiles install` on the review host to write the new Codex adapter; the recorded failure and the notifications keep only the code.
