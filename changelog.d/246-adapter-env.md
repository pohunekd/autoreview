---
kind: security
breaking: true
---
The `--version` probe of the Claude and Codex adapters runs the agent executable with the environment of a review run, without the pohunek session identity, the GitHub and Matrix tokens and the `AUTOREVIEW_` variables. The adapters' `render_profile.py` copies only the telemetry keys (`OTEL_*`, and `CLAUDE_CODE_ENABLE_TELEMETRY` for Claude) and the config home (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) of the base profile's `[env]`, drops every other key and prints the dropped names on stderr, so a profile rendered again loses any other variable it carried. Run `autoreview profiles install` for every Claude and Codex profile after the upgrade, and render again every profile written with `render_profile.py`.
