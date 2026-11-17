---
kind: removed
breaking: true
---
The engine no longer reads the Claude result name `claude-otel-noninteractive/v2` or the `pohunek-codex-review` Codex markers, and the adapters refuse the `claude-otel-noninteractive/request-v1` request and a Codex prompt without a protocol line. Run `autoreview profiles install` for every Claude and Codex profile on every review host with this release before you upgrade the engine; a host that still runs an older Claude adapter stops its reviews with `review_adapter_outdated`, and an older Codex adapter ends them in `review_nonce_mismatch` after the fresh-session retries.
