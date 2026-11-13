---
kind: added
---
The release archive carries `deploy/install.sh` and hardened systemd units that run autoreview as the system user `autoreview`. The installer can run again without changing anything that is in place, runs `validate` as that user before it replaces the binary or a unit or restarts the service, and the units pass the secrets only through `--env-file`.
