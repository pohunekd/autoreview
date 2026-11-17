---
kind: changed
---
`scripts/setup` on macOS writes a `log:` block to the new `config.yaml` and points launchd at a small `.stderr.log` file, so autoreview rotates its logs without newsyslog or a restart; an existing `config.yaml` without the block makes setup print it and stop. `scripts/macos-check service` checks that the review agent's log exists and that no log file passes `max_file_bytes` plus one line.
