---
kind: changed
---
When `scripts/setup` upgrades an autoreview older than 0.4.0, it names each Codex profile that `autoreview profiles install` wrote earlier and prints the `profiles install` command that writes its adapter again, because 0.4.0 refuses every request to an earlier Codex adapter with `review_nonce_mismatch`. Setup keeps those profiles and does not know the flags of their first install, so the command holds placeholders for them.
