---
kind: security
---
A process that a Codex review run leaves behind can no longer write after the Codex adapter's result markers: when Codex exits, the adapter ends every process below it, also one that forked twice or started its own session (`SIGTERM`, then `SIGKILL` after 2 seconds), and stops copying Codex's stderr before it prints a marker with the run's nonce. The fix is in the adapter that `autoreview profiles install` writes, so run it for every Codex profile after the update. Claude profiles need nothing.
