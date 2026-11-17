---
kind: changed
---
`scripts/setup` asks for the notifier, `none`, `matrix` or `webhook` (or takes `--notifier NAME`), then the three Matrix values only for `matrix` and the webhook format `json`, `slack` or `discord` only for `webhook` (or `--webhook-format FORMAT`), so a setup without Matrix needs no edit of `config.yaml`; the Matrix flags and `--webhook-format` are refused with the other notifiers. Setup installs autoreview 0.7.0, the first release whose `init` knows `--notifier`.
