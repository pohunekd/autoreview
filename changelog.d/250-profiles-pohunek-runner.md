---
kind: changed
breaking: true
---
`autoreview profiles install` requires `--pohunek-timeout-seconds` and kills a pohunek call that runs longer. Its pohunek calls run as the engine's do: with `PAGER=cat` and without the pohunek session identity, the GitHub and Matrix token variables and the `AUTOREVIEW_` variables, and an answer that is not a JSON object, or that has a key differing from a known key only in letter case, is refused with `pohunek_command_failed`.
