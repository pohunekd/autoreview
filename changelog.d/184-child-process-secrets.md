---
kind: security
---
The `gh` and `pohunek` processes that autoreview starts no longer inherit any `AUTOREVIEW_*`
variable, any variable named by a `*_env` key or the Go engine's `MATRIX_ACCESS_TOKEN`; a `gh`
call still gets the token of its credential as `GH_TOKEN`.
