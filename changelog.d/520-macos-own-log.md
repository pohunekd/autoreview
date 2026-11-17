---
kind: changed
---
`scripts/setup` on macOS writes a `log:` block to the new `config.yaml` and points launchd at a small `.stderr.log` file, so autoreview rotates its logs without newsyslog or a restart; an existing `config.yaml` without the block makes setup print it and stop. `scripts/macos-check service` checks that the log files exist and stay within `max_file_bytes`.
