---
kind: fixed
---
`scripts/setup` checks in its detection step, before it downloads anything, every directory and file that pohunek refuses because other users can change it (the config, data and state directories, `~/.local/bin`, the unit directory and the config home of each agent on the `PATH`), lists all of them at once with a `chmod go-w` command for each writable one and stops with exit code 3; a config home that pohunek refuses in the hooks step is reported with pohunek's reason instead of as missing. Each verified attestation prints one line naming the archive.
