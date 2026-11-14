---
kind: security
---
`deploy/install.sh --root DIR` refuses a DIR, or a directory under it that holds an installed path, that a user other than you or root owns or that group or others can write to, and prints a `chmod go-w` command for a writable one. Such a user could otherwise swap a directory for a symlink after the check and redirect a write outside DIR.
