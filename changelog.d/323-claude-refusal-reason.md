---
kind: changed
---
When the Claude adapter refuses a run for any reason but a missing head, the engine logs its message, bounded and neutralized, with `review_result_invalid` in the `error` field of the reviewer output warnings; every Claude adapter already sends the message, so no `autoreview profiles install` is needed, and the recorded failure and the notifications keep only the code.
