---
kind: fixed
---
The Codex adapter switches its terminal to raw mode before it sets its ready title, so a request that pohunek writes right after the title is no longer discarded and the review no longer waits for input that never comes. Run `autoreview profiles install` on the review host to write the new adapter.
