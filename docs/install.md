---
type: Reference
id: reference/install
title: Install autoreview
description: Prerequisites, the four install paths (release archive with checksum and attestation, systemd units, container image, pohunek workflow plugin planned in #41), building from source, first configuration, the first review and where to go for upgrades.
source_kind: manual
intents: [setup, help]
---

# Install autoreview

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change. The release archives are built by the release workflow of `pohunekd/autoreview` from the first version tag on; no release is published yet. The container image `ghcr.io/pohunekd/autoreview` is planned for v0.1 and is not published yet.

autoreview runs as one service with the GitHub CLI and the `pohunek` CLI. The
agents run as reviewers in pohunek sessions on the **review hosts**, where a
pohunek daemon runs and the repositories are registered.

## Prerequisites

- **pohunek daemon on each review host** (the minimum version is not published
  yet; a version check in `validate` is planned in #9).
- **Runtime packages** `claude` and/or `codex`, installed on each review host per the
  [pohunek runtime packages guide](https://pohunek.dev/docs/guides/runtime-packages/).
- **Repositories registered** with `pohunek project add` on a review host. The first
  host of `pohunek.hosts` that has the repository runs its reviews, unless
  `pohunek.route_overrides` pins it ([multiple hosts](knowledge/guides/multihost.md)).
- **GitHub CLI (`gh`)** on the autoreview host (the container image bundles it).
- **GitHub token** of the account autoreview acts as, held in the variable named
  by the `token_env` of the one credential (`AUTOREVIEW_GH_TOKEN` in the
  example). It belongs to the credential's `login`; several credentials are
  planned in #13.
  A personal token or a machine user works; see [credentials](configuration.md#credentials)
  and [GitHub identity](knowledge/concepts/github-identity.md).
- **Git access on each review host.** The review hosts fetch the code with their own
  git credentials, not with these tokens, so a private repository must also be
  reachable from the review host.

## Choose an install path

| Path | Use it when | You get |
|---|---|---|
| a. Release binary (default) | You run the service yourself under a dedicated server user | `autoreview` in `/usr/local/bin` |
| b. systemd units | You want the service and the hourly queue as units under a dedicated server user | `autoreview.service`, `autoreview-queue.service`, `autoreview-queue.timer` |
| c. Container | You run containers | `ghcr.io/pohunekd/autoreview:<version>` (planned) |
| d. pohunek workflow plugin | Not available yet | Planned in #41 |

Paths a and b need the `autoreview` system user that path b creates.

## a. Release binary

The release archive is the default install path. Each release of
`pohunekd/autoreview` carries a static Linux binary per target,
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, as
`autoreview-<version>-<target>.tar.gz`, with its `.sha256` file and a build
provenance attestation. The binary needs no system library.

Download the archive and its checksum file, then check the checksum:

```sh
version=0.1.0                        # the release to install, without the v
target=x86_64-unknown-linux-musl     # or aarch64-unknown-linux-musl
archive="autoreview-$version-$target.tar.gz"
curl -fLO "https://github.com/pohunekd/autoreview/releases/download/v$version/$archive"
curl -fLO "https://github.com/pohunekd/autoreview/releases/download/v$version/$archive.sha256"
sha256sum --check "$archive.sha256"
```

With the GitHub CLI, also check the attestation. It proves that the release
workflow of this repository built the archive:

```sh
gh attestation verify "$archive" --repo pohunekd/autoreview \
  --signer-workflow pohunekd/autoreview/.github/workflows/release.yml
```

Unpack the archive and install the binary:

```sh
tar -xzf "$archive"
sudo install -m 0755 "autoreview-$version-$target/autoreview" /usr/local/bin/autoreview
autoreview --version
```

### Build from source

Build from a checkout of the release tag when no archive fits your platform.
rustup installs the toolchain of `rust-toolchain.toml` on the first cargo call.

```sh
git clone --branch "v$version" https://github.com/pohunekd/autoreview
cd autoreview
cargo build --release --locked -p autoreview
sudo install -m 0755 target/release/autoreview /usr/local/bin/autoreview
```

This binary links the C library of the build host. The static binary of the
release needs the musl target and a musl C compiler (`musl-tools` on Debian
and Ubuntu):

```sh
rustup target add x86_64-unknown-linux-musl
TARGET_CC=musl-gcc cargo build --release --locked --target x86_64-unknown-linux-musl -p autoreview
```

## b. systemd units

```sh
sudo useradd --system --home-dir /var/lib/autoreview --shell /usr/sbin/nologin autoreview
sudo install -d -o root -g root -m 0755 /etc/autoreview /etc/autoreview/prompts
```

Create the environment file (the variable of the credential's `token_env` and the
backend secrets), write the values in an editor, and never on a command line. See [secrets](knowledge/safety/secrets.md).

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

Not available yet: the repository contains no workflow plugin. Planned in #41.

## First configuration

Copy `examples/config.yaml`, a policy from the
[examples](../examples/README.md) and the prompts of `prompts/` from a checkout
of this repository. `autoreview init` is planned in #8.

```sh
sudo install -d /etc/autoreview/prompts
sudo cp examples/config.yaml /etc/autoreview/config.yaml
sudo cp examples/single-repo-comment/policy.yaml /etc/autoreview/policy.yaml
sudo cp prompts/generic.md prompts/verifier.md /etc/autoreview/prompts/
```

In `config.yaml`, set the `login` of the credential, `pohunek.hosts`, and the
homeserver, room and mentioned user of the placeholder `matrix` backend in
`notify`, see [notifiers](knowledge/guides/notifiers.md).

The agent profile named as `reviewers.<id>.agent_profile` in `policy.yaml` must
exist on the review host; autoreview does not create it. `autoreview profiles
install` is planned in #25.

Check configuration, policy and prompts before every start:

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
login of the credential, or add the `label:` trigger's label.

## Upgrade

Before you replace the binary or the image, read
[upgrade and backup](knowledge/runbooks/upgrade-and-backup.md).
