---
kind: added
---
Each release pushes the attested `linux/amd64` image `ghcr.io/pohunekd/autoreview:<version>`, which runs as a non-root user with `gh` and the pohunek CLI at pinned versions, reads the configuration from a read-only `/etc/autoreview` and keeps its state in the `/var/lib/autoreview` volume; `deploy/compose/compose.yml` runs it.
