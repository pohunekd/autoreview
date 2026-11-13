---
kind: changed
---
A failed discovery search now also logs an error that names its credential and error code; the pass still fails as before, and the drafts warning names the credential too. With one credential narrower than `*`, a waiting approval of a repository that the credential no longer serves ends instead of failing every pass, and such a notes hand-off ends instead of being read again on every pass.
