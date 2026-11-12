---
kind: security
---
The `gh` and `pohunek` processes that autoreview starts inherit only an allowlist of the service's variables (`PATH`, `HOME`, `TMPDIR`, the `XDG_*` directories, and the config, proxy, CA and telemetry variables of `gh` or the remote port of `pohunek`), so a token of another tool in a shared environment file no longer reaches them. Every other variable, for example `GITHUB_TOKEN`, `GH_HOST`, `GH_DEBUG`, `LANG` or `TZ`, is no longer passed; see the secrets page for the full list.
