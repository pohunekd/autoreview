---
kind: fixed
---
A reviewer session whose worker pohunek reports as lost during the start now fails with the retryable code `pohunek_session_lost`, logged with pohunek's loss cause, instead of `session_identity_invalid` or `pohunek_session_cleanup_failed`. The explanations of `pohunek_base_fetch_timeout` and `pohunek_base_fetch_authentication_failed` name the SSH agent, and the troubleshooting runbook gains checks for inotify watches, non-interactive Git access and fetching over HTTPS.
