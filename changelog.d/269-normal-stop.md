---
kind: fixed
---
`scripts/setup` starts `autoreview.service` once (`enable`, then one `restart`) instead of starting it and restarting it at once, and a SIGINT or SIGTERM during the GitHub login check of `autoreview serve` is a normal stop: no `ERROR` line, exit status 0, and the unit is not marked failed.
