---
kind: security
---
Codex reviewer runs now pass `project_doc_max_bytes=0`, so Codex reads no `AGENTS.md` file of the review checkout as an instruction: before, the `AGENTS.md` files that a `checkout: head` pull request adds or changes became instructions of the review run. The reviewer still reads the repository's guidance from the trusted base as the prompt tells it. A `.codex/config.toml` in the checkout is not loaded either, because Codex reads it only for a project that the host's system or managed configuration trusts; the docs now say so. The change reaches a review host only when its profiles are installed again with `autoreview profiles install`.
