---
kind: changed
---
autoreview now bundles SQLite 3.54.1 through `rusqlite` 0.41, which fixes RUSTSEC-2026-0459 (uninitialized memory read from a crafted write-ahead log). In both supported deployments autoreview opens only its own store, in a directory that no other user may write, so the advisory did not reach it; existing store databases open unchanged.
