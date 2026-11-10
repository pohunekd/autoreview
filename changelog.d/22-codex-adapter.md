---
kind: added
---
The Codex adapter ships in `adapters/codex/`: a wrapper that runs one bounded, read-only Codex review for a pohunek session and prints a result frame, and a renderer that writes its pohunek profile. The Codex path comes from configuration, telemetry is optional, and the report now carries the blocking flag of each finding and verdicts on prior findings, which the engine reads from a later release.
