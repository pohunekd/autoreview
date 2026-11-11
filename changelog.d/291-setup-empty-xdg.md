---
kind: fixed
---
`scripts/setup` stops in its detection step with exit code 3 and names the variable when `XDG_CONFIG_HOME`, `XDG_STATE_HOME` or `XDG_DATA_HOME` is set but empty, which pohunek refuses, instead of at a later pohunek call. An unset variable still means the default under the home directory.
