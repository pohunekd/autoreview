---
kind: changed
---
`serve` fails at startup with `credential_login_mismatch` instead of `configuration_invalid` (naming `github.credentials.login`) when a token belongs to another account than its credential's `login`, and with `credential_invalid` instead of `github_request_failed` when GitHub refuses a token.
