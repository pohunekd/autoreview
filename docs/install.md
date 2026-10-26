---
type: Reference
id: reference/install
title: Install autoreview
description: Prerequisites, the four install paths (release binary, systemd units, container image, pohunek workflow plugin), first configuration, the first review and where to go for upgrades.
source_kind: manual
intents: [setup, help]
---

# Install autoreview

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change. The release archives (`pohunekd/autoreview` GitHub releases) and the container image `ghcr.io/pohunekd/autoreview` are planned for v0.1 and are not published yet.

autoreview runs as one service with the GitHub CLI and the `pohunek` CLI. The
agents run as reviewers in pohunek sessions on the **review hosts**, where a
pohunek daemon runs and the repositories are registered.

## Prerequisites

- **pohunek daemon on each review host**, at a version `autoreview validate` accepts
  (the minimum version is not published yet).
- **Runtime packages** `claude` and/or `codex`, installed on each review host per the
  [pohunek runtime packages guide](https://pohunek.dev/docs/guides/runtime-packages/).
- **Repositories registered** with `pohunek project add` on exactly one review host,
  unless pinned with `pohunek.route_overrides` ([multiple hosts](knowledge/guides/multihost.md)).
- **GitHub CLI (`gh`)** on the autoreview host (the container image bundles it).
- **GitHub tokens** of the accounts autoreview acts as: one token per credential in
  `github.credentials`, held in the variable named by its `token_env` (for a single
  credential, `AUTOREVIEW_GH_TOKEN`). Each token belongs to its credential's `login`.
  A personal token or a machine user works; see [credentials](configuration.md#credentials)
  and [GitHub identity](knowledge/concepts/github-identity.md).
- **Git access on each review host.** The review hosts fetch the code with their own
  git credentials, not with these tokens, so a private repository must also be
  reachable from the review host.

## Choose an install path

| Path | Use it when | You get |
|---|---|---|
| a. Release binary | You run the service yourself under a dedicated server user | `autoreview` in `/usr/local/bin` |
| b. systemd units | You want the service and the hourly queue as units under a dedicated server user | `autoreview.service`, `autoreview-queue.service`, `autoreview-queue.timer` |
| c. Container | You run containers | `ghcr.io/pohunekd/autoreview:<version>` (planned) |
| d. pohunek workflow plugin | The machine already runs pohunek as the account that owns the review | the plugin `pohunekd.autoreview`: a service, a job and actions |

Paths a and b need the `autoreview` system user that path b creates. Path d runs as
the pohunek owner user and creates no system user.

## a. Release binary

Download the archive for your platform and its `.sha256` file from the release
of `pohunekd/autoreview`, then check both the checksum and the build attestation:

```sh
gh release download <tag> --repo pohunekd/autoreview --pattern '<archive-for-your-platform>' --pattern '<archive-for-your-platform>.sha256'
sha256sum --check <archive-for-your-platform>.sha256
gh attestation verify <archive-for-your-platform> --repo pohunekd/autoreview
tar -xzf <archive-for-your-platform>
sudo install -m 0755 autoreview /usr/local/bin/autoreview
```

Asset names are not fixed yet; use the names the release page shows.

## b. systemd units

```sh
sudo useradd --system --home-dir /var/lib/autoreview --shell /usr/sbin/nologin autoreview
sudo install -d -o root -g root -m 0755 /etc/autoreview /etc/autoreview/prompts
```

Create the environment file (the `AUTOREVIEW_GH_TOKEN*` variables, one per credential's
`token_env`, and backend secrets), write the values in an editor, and never on a command line. See [secrets](knowledge/safety/secrets.md).

```sh
sudo install -m 0600 -o root -g root /dev/null /etc/autoreview/autoreview.env
```

Both units set `RuntimeDirectoryPreserve=yes`, so the hourly oneshot cannot remove `/run/autoreview` while `serve` runs.

`/etc/systemd/system/autoreview.service`:

```ini
[Unit]
Description=autoreview pull request review service
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=autoreview
Group=autoreview
ExecStart=/usr/local/bin/autoreview --config /etc/autoreview/config.yaml serve
EnvironmentFile=/etc/autoreview/autoreview.env
StateDirectory=autoreview
RuntimeDirectory=autoreview
RuntimeDirectoryPreserve=yes
Restart=on-failure
RestartSec=10
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/autoreview /run/autoreview
PrivateTmp=yes
CapabilityBoundingSet=

[Install]
WantedBy=multi-user.target
```

`/etc/systemd/system/autoreview-queue.service`, started by the timer:

```ini
[Unit]
Description=autoreview review queue notice

[Service]
Type=oneshot
User=autoreview
Group=autoreview
ExecStart=/usr/local/bin/autoreview --config /etc/autoreview/config.yaml review-queue
EnvironmentFile=/etc/autoreview/autoreview.env
StateDirectory=autoreview
RuntimeDirectory=autoreview
RuntimeDirectoryPreserve=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/autoreview /run/autoreview
PrivateTmp=yes
CapabilityBoundingSet=
```

`/etc/systemd/system/autoreview-queue.timer`, hourly from 08:00 to 20:00 host
local time:

```ini
[Unit]
Description=Hourly autoreview review queue, 08:00-20:00 local time

[Timer]
OnCalendar=*-*-* 08..20:00:00
Unit=autoreview-queue.service

[Install]
WantedBy=timers.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now autoreview.service autoreview-queue.timer
```

The `autoreview` user needs a route to each review host (for example NetBird) and
the pohunek client access the CLI needs. That setup is not in the reference pages yet.

## c. Container

The image bundles `gh` and the `pohunek` CLI at pinned versions. The config is
mounted read-only, state goes to a volume, and the container must reach the review
hosts. This example uses host networking to share the host's tunnel.

`compose.yml`:

```yaml
services:
  autoreview:
    image: ghcr.io/pohunekd/autoreview:<version>   # planned, not published yet
    command: ["--config", "/etc/autoreview/config.yaml", "serve"]
    env_file: ./autoreview.env                     # mode 0600, holds the AUTOREVIEW_GH_TOKEN* variables and other AUTOREVIEW_* secrets
    volumes:
      - ./config:/etc/autoreview:ro
      - autoreview-state:/var/lib/autoreview
    tmpfs:
      - /run/autoreview
    read_only: true
    security_opt:
      - no-new-privileges:true
    network_mode: host
    restart: unless-stopped

volumes:
  autoreview-state:
```

Set `github.gh_bin` and `pohunek.bin` to the paths of the bundled tools inside
the image. The image layout is not documented yet.

## d. pohunek workflow plugin

autoreview is also distributed as the pohunek workflow plugin `pohunekd.autoreview`.
Use it on a machine that already runs pohunek as the account that owns the review.
pohunek installs the service and the job as user service units of that owner, so the
plugin runs as the owner user, not as a dedicated system user. See the
[pohunek workflow plugin guide](https://pohunek.dev/docs/guides/workflow-plugins/)
for how plugins are installed, verified and run.

Install the plugin archive `autoreview-<version>.tar.zst` from the release, with
the digest published next to it. Run the command without `--yes` first to review
what the plugin declares, then repeat it with `--yes`:

```sh
pohunek workflow install autoreview-<version>.tar.zst --sha256 <digest>
pohunek workflow install autoreview-<version>.tar.zst --sha256 <digest> --yes
```

The plugin declares:

- a **service** that runs `serve`;
- a **job** that sends the hourly review queue (`review-queue`);
- an **action** for every other command of the [CLI reference](cli.md): `init`,
  `validate`, `profiles-install`, `poll`, `candidates`, `reviews`, `review`,
  `awaiting`, `approve`, `resolve`, `retry`, `prune-branches` and `backup-db`.

Every action runs the same `autoreview` binary through the plugin's wrapper,
which passes the plugin's own files explicitly:

```
autoreview --config  "$POHUNEK_PLUGIN_CONFIG_DIR/config.yaml" \
           --env-file "$POHUNEK_PLUGIN_CONFIG_DIR/secrets.env" <command> ...
```

pohunek starts plugin processes with an empty environment: no `PATH` and no
tokens. That is why the tokens live in `secrets.env` and why `github.gh_bin`
and `pohunek.bin` are absolute paths; for the plugin, `pohunek.bin` must be the
CLI pohunek hands the plugin (`validate` checks it). Both files are in the
plugin's owner-private configuration directory, which survives uninstall
(`uninstall --purge` removes it). Find it with:

```sh
pohunek workflow inspect pohunekd.autoreview
```

Set it up with the `init` action, then put the tokens in `secrets.env`
(mode `0600`, one `AUTOREVIEW_...=...` line per token, edited in a text editor,
never echoed into a shell history), then validate:

```sh
pohunek workflow run pohunekd.autoreview init --login <github-login> --pohunek-host <host> --example single-repo-comment
$EDITOR <plugin config dir>/secrets.env
pohunek workflow run --json pohunekd.autoreview validate
pohunek workflow run --json pohunekd.autoreview approve --repository <owner/repo> <number> <head-sha>
```

For the plugin, `init` writes into the plugin's configuration directory and
sets `store.path` inside the plugin's state directory; `--dir` is not used.
`backup-db` needs an absolute target path outside the plugin's directories.

## First configuration

Write `config.yaml`, a policy and the prompts from an example. The command
refuses to overwrite existing files. `--login` becomes the login of the one credential
that `init` writes, which reads its token from `AUTOREVIEW_GH_TOKEN`; add credentials
for other accounts by hand, see [credentials](configuration.md#credentials).

```sh
sudo autoreview init --dir /etc/autoreview --login <github-login> --pohunek-host <host> --example single-repo-comment
```

Create the agent profile on the review host, named as `reviewers.<id>.agent_profile` in `policy.yaml`:

```sh
sudo -u autoreview autoreview --config /etc/autoreview/config.yaml profiles install --host <host> --agent claude --model <model-id> --name autoreview-opus
```

Check configuration, policy, prompts and pohunek version before every start:

```sh
sudo -u autoreview autoreview --config /etc/autoreview/config.yaml validate
```

The output is one JSON object; error messages are listed in [policy validation errors](policy.md#validation-errors).

## First review

Start the service, then check what discovery picks up and run one pass. A review
starts after `schedule.debounce_seconds`.

```sh
autoreview --config /etc/autoreview/config.yaml candidates
autoreview --config /etc/autoreview/config.yaml poll
autoreview --config /etc/autoreview/config.yaml reviews
autoreview --config /etc/autoreview/config.yaml review --repository <owner/repo> <number>
```

Open a test pull request in a matching repository and request a review from the
login of the credential that serves the repository, or add the `label:` trigger's label.

## Upgrade

Before you replace the binary or the image, read
[upgrade and backup](knowledge/runbooks/upgrade-and-backup.md).
