---
kind: added
---
`scripts/setup` sets up autoreview on one Linux machine that also runs the pohunek daemon: it installs autoreview and pohunek from their release archives after checking each checksum and attestation, writes the configuration with `autoreview init`, creates the secrets file, runs `validate`, installs the runtime packages, the hooks and the agent profiles the policy needs, and installs the user service, asking before every change and skipping every step that is in place. `autoreview init` takes optional flags for the store, `gh` and pohunek paths and the Matrix backend; without them it writes the reference values. A new guide, "Getting started", walks through the script, and the multi-host guide describes the deployment layout.
