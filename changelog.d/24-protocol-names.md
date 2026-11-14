---
kind: changed
---
The reviewer protocols are named `autoreview-claude/v1` and `autoreview-codex/v1` between the engine and the adapters: a Codex adapter written by `autoreview profiles install` answers in the new markers, and every adapter still answers an older engine in the legacy names, which the engine keeps reading. Engine updates and rollbacks need no profile reinstall; the engine keeps sending Claude requests under the legacy name until every review host runs the new adapter.
