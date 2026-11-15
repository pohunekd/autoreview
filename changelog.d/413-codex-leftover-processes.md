---
kind: security
---
A process that a Codex review run leaves behind can no longer write after the Codex adapter's result markers: when Codex exits, the adapter ends every process below it, also one that forked twice or started its own session (the adapter's children get `SIGTERM`, every process still left gets `SIGKILL` after 2 seconds), and stops copying Codex's stderr before it prints a marker with the run's nonce. The fix is in the adapter that `autoreview profiles install` writes, which now needs Linux (`PR_SET_CHILD_SUBREAPER`) and exits with status 64 elsewhere, so run it for every Codex profile after the update. Claude profiles need nothing.
