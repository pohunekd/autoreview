---
kind: fixed
---
`autoreview profiles install` refuses an `XDG_CONFIG_HOME` or `XDG_DATA_HOME` with an empty, `.` or `..` part or a trailing `/`, and such a `HOME` when the variable is unset, with `configuration_invalid` naming the variable, as pohunek refuses such a path, instead of writing the profile and the adapter under a path that a pohunek daemon started in the same environment refuses.
