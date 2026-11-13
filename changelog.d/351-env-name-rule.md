---
kind: changed
---
Every name in a `*_env` key of `config.yaml` (`token_env`, `access_token_env`, `url_env`) must match `AUTOREVIEW_[A-Z0-9_]+`, the rule of a name in an `--env-file`. A configuration with another name, for example one with a lowercase letter or a hyphen, fails with `configuration_invalid` naming the key instead of reading the variable from the environment.
