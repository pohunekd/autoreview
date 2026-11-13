---
kind: added
---
`serve` checks every credential's token again while it runs: once when the engine starts and then every `github.credential_check_hours`. A token that expires within `github.expiry_warning_days` raises the new `credential_expiring` event, at most once a UTC day per credential while `serve` runs (a restart warns again); the message names the credential and the expiry. The event is optional in the policy and defaults to `action`, so existing policies keep loading with the same digest. `github.credential_check_hours` above 8760 and `github.expiry_warning_days` above 366 are refused as `configuration_invalid`.
