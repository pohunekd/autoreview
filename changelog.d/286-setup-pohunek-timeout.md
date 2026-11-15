---
kind: changed
---
`scripts/setup` installs autoreview 0.4.0 and passes `--pohunek-timeout-seconds` to every `autoreview profiles install` it runs. It takes the value from the new `--pohunek-timeout-seconds` flag, or asks once when a profile is missing; there is no default, and a value that is not a whole number of seconds above 0 stops the script with exit code 1.
