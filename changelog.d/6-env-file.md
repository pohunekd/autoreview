---
kind: added
---
`--env-file <path>` gives the secrets of the `*_env` keys in a file of `NAME=value` lines, for a caller that starts autoreview with an empty environment, such as a pohunek workflow plugin. The file must be owned by the user that runs autoreview and give the group and others no access; a name in the file wins over the same environment variable, and its values are never added to the environment of autoreview, so no child process inherits them.
