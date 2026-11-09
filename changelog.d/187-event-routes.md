---
kind: added
---
`notify.routes` in `config.yaml` accepts an event name as a key next to `action`, `digest` and
`log`, so a single event, such as `approved`, posts to its own Matrix room while every other event
keeps the route of its level. The level still decides the mention and whether a failed message is
dropped. A key that names no event, or a backend that is not configured, is refused at load with
`configuration_invalid` naming `notify.routes.<key>`.
