---
kind: fixed
---
`autoreview profiles install` refuses a set but empty `XDG_CONFIG_HOME` or `XDG_DATA_HOME` with `configuration_invalid` naming the variable, as pohunek refuses it, instead of writing the profile and the adapter under the defaults in the home directory while a pohunek daemon started in the same environment refuses to start. An unset variable still means the default under the home directory.
