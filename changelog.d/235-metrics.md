---
kind: added
---
`autoreview serve` answers `GET /metrics` in the Prometheus text format when `config.yaml` has a `metrics` section: gauges of the queued, running, awaiting and stopped reviews, and counters and a publication time histogram of the reviews it runs, on the numbers of `autoreview stats`. Without the section `serve` opens no port.
