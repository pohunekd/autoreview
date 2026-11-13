---
kind: changed
---
A review reads its pull request and publishes with the credential that served its repository when it was dispatched, also after a restart or a config change. A review whose credential was removed or renamed, no longer matches the repository or now has another login stops with `credential_not_covered` instead of posting with another account; `autoreview retry` reviews the head again with the credential that serves it now.
