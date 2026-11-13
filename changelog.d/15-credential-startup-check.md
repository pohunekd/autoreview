---
kind: added
---
`serve` checks every credential's token with GitHub before it starts and logs each token's expiry; a token that GitHub refuses fails with `credential_invalid` and a token of another account with `credential_login_mismatch`, each naming the credential. `validate --check-credentials` runs the same check and prints each credential's `name`, `login` and `expires_at`; without the option, `validate` still makes no GitHub call.
