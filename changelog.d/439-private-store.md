---
kind: security
breaking: true
---
`autoreview validate` refuses a store whose directory or files grant the group or other users any access, with `configuration_invalid` naming `store.path` and the `chmod 700` or `chmod 600` that fixes each path; it changes no mode itself. autoreview creates a missing store database with mode 0600 whatever the umask, `autoreview init` creates its `--dir` with mode 0700, and `scripts/setup` runs `init` under `umask 077` and sets `UMask=0077` in its user units, so the store of a new setup deployment is private to the user. A setup deployment made earlier under the common umask 022 has a 0755 directory and a 0644 store that other local users can read: run the printed `chmod` commands, or `validate`, and with it setup, stops.
