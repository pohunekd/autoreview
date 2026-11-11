---
kind: fixed
---
`autoreview retry`, `resolve` and `approve`, and the MCP tools of the same names, refuse a malformed head with `head_prefix_invalid` (a prefix must be 12 to 40 lowercase hexadecimal characters) or `head_sha_invalid` (`approve` needs the full 40-character head) instead of `review_identity_invalid`, and the CLI message names the argument and the accepted form.
