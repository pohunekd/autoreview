---
type: Reference
id: reference/install
title: Install autoreview
description: Prerequisites, the three install paths (release archive with checksum and attestation, systemd units with their installer, container image), building from source, first configuration, the first review and where to go for upgrades.
source_kind: manual
intents: [setup, help]
---

# Install autoreview

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0. The release archives are
> published from `v0.1.0` on; the release workflow builds and attests them.
> The systemd units and their installer of path b come in the archive after
> 0.3.0, and so does the container image `ghcr.io/pohunekd/autoreview` of
> path c.

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
  planned in #14 to #17.
  A personal token or a machine user works; see [credentials](configuration.md#credentials)
  and [GitHub identity](knowledge/concepts/github-identity.md).
- **Git access on each review host.** The review hosts fetch the code with their own
  git credentials, not with these tokens, so a private repository must also be
  reachable from the review host.

## Choose an install path

To run autoreview and the pohunek daemon together on one Linux machine as your
own user, run `scripts/setup` (after 0.2.0) from a checkout of the repository: it installs
both from their release archives, writes the configuration and the user
service, and asks before every change. See
[getting started](knowledge/guides/getting-started.md). The paths below are for
a dedicated server user.

| Path | Use it when | You get |
|---|---|---|
| a. Release binary (default) | You run the service yourself under a dedicated server user | `autoreview` in `/usr/local/bin` |
| b. systemd units | You want the service and the hourly queue as units under a dedicated server user | `deploy/install.sh`, which installs `autoreview.service`, `autoreview-queue.service`, `autoreview-queue.timer` |
| c. Container | You run containers on `linux/amd64` | `ghcr.io/pohunekd/autoreview:<version>` and `deploy/compose/compose.yml` |

Paths a and b need the `autoreview` system user that path b creates.

## a. Release binary

The release archive is the default install path. Each release of
`pohunekd/autoreview` carries a static Linux binary per target,
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, as
`autoreview-<version>-<target>.tar.gz`, with its `.sha256` file and a build
provenance attestation. The binary needs no system library.

Download the archive and its checksum file, then check the checksum:

```sh
version=0.2.0                        # the release to install, without the v
target=x86_64-unknown-linux-musl     # or aarch64-unknown-linux-musl
archive="autoreview-$version-$target.tar.gz"
curl -fLO "https://github.com/pohunekd/autoreview/releases/download/v$version/$archive"
curl -fLO "https://github.com/pohunekd/autoreview/releases/download/v$version/$archive.sha256"
sha256sum --check "$archive.sha256"
```

With the GitHub CLI, also check the attestation. It proves that the release
workflow of this repository built the archive from the release tag on a
GitHub-hosted runner; an attestation from a run on another branch or ref does
not pass:

```sh
gh attestation verify "$archive" --repo pohunekd/autoreview \
  --signer-workflow pohunekd/autoreview/.github/workflows/release.yml \
  --source-ref "refs/tags/v$version" --deny-self-hosted-runners
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

After 0.3.0, the release archive of path a also holds `deploy/install.sh` and
the units it installs:
[`autoreview.service`](../deploy/systemd/autoreview.service),
[`autoreview-queue.service`](../deploy/systemd/autoreview-queue.service) and
[`autoreview-queue.timer`](../deploy/systemd/autoreview-queue.timer), which
sends the review queue hourly from 08:00 to 20:00 host local time. Download,
check and unpack the archive as in path a, then run the installer as root:

```sh
sudo "autoreview-$version-$target/deploy/install.sh" \
  --binary "autoreview-$version-$target/autoreview"
```

It creates the system user `autoreview`, installs the binary as
`/usr/local/bin/autoreview`, creates `/etc/autoreview` (mode `0755`), the state
directory `/var/lib/autoreview` (mode `0700`, owned by `autoreview`) and the
empty secrets file `/etc/autoreview/autoreview.env`, and writes the units to
`/etc/systemd/system/`. A file that is in place is not written again, so a
second run changes nothing.

Until `/etc/autoreview/config.yaml` exists, it enables and starts nothing.
Write the configuration as in [first configuration](#first-configuration) and
the secrets as below, then run the installer again. From then on, each run
checks the configuration with `validate`, as the `autoreview` user and with the
new binary, before it replaces the binary or a unit; when `validate` fails, it
stops and the running service stays as it was. Then it enables `autoreview.service` and
`autoreview-queue.timer`, and restarts the service when the binary or a unit
changed or when it is not running. Its exit codes: 1 usage, 2 not root or not
an autoreview binary, 3 the secrets file, 4 `validate`, 5 a failed install
step.

The secrets file holds the variable of the credential's `token_env` and the
backend secrets as `NAME=value` lines. Write the values with an editor, never
on a command line, for example with `sudoedit /etc/autoreview/autoreview.env`.
See [secrets](knowledge/safety/secrets.md). The units pass the file with
`--env-file`, so it must be owned by `autoreview` with mode `0600`; the
installer checks the owner and the mode, prints the command that fixes them,
and never reads the values.

The `gh` and `pohunek` processes that autoreview starts inherit only the
variables listed in [secrets](knowledge/safety/secrets.md#what-child-processes-receive);
any other variable of the unit, such as a token of another tool, stays with
autoreview.

The units run the service without capabilities, with a read-only file system
except `/var/lib/autoreview` and `/run/autoreview`, without access to home
directories, with a private `/tmp` and a system call filter. Both units set
`RuntimeDirectoryPreserve=yes`, so the hourly oneshot cannot remove
`/run/autoreview` while `serve` runs.

The `autoreview` user needs a route to each review host (for example NetBird) and
the pohunek client access the CLI needs. That setup is not in the reference pages yet.

## c. Container

After 0.3.0, each release pushes `ghcr.io/pohunekd/autoreview:<version>` for
`linux/amd64`, built from the x86_64 release archive and attested by the
release workflow. There is no `arm64` image: autoreview cannot review without
the `pohunek` CLI, and pohunek publishes its Linux CLI for x86_64 only. Check
the attestation of the image before you run it:

```sh
gh attestation verify "oci://ghcr.io/pohunekd/autoreview:$version" --repo pohunekd/autoreview \
  --signer-workflow pohunekd/autoreview/.github/workflows/release.yml \
  --source-ref "refs/tags/v$version" --deny-self-hosted-runners
```

The image holds autoreview as `/usr/local/bin/autoreview`, `gh` as
`/usr/bin/gh` and the `pohunek` CLI as `/usr/local/bin/pohunek`, at the versions
pinned in [`deploy/image/Dockerfile`](../deploy/image/Dockerfile). These are the
paths of the complete configuration file, so a configuration written by `init`
runs unchanged. It runs as the user `65532` with `HOME=/var/lib/autoreview` and
`AUTOREVIEW_CONFIG=/etc/autoreview/config.yaml`; the config is mounted
read-only, state goes to a volume, and the container must reach the review
hosts. The example uses host networking to share the host's tunnel.

[`deploy/compose/compose.yml`](../deploy/compose/compose.yml):

```yaml
services:
  autoreview:
    image: "ghcr.io/pohunekd/autoreview:${AUTOREVIEW_VERSION:?set the release to run}"
    command: ["serve"]
    # Holds the AUTOREVIEW_* secrets of the *_env keys of config.yaml.
    env_file: ./autoreview.env
    volumes:
      - ./config:/etc/autoreview:ro
      - autoreview-state:/var/lib/autoreview
    tmpfs:
      - /run/autoreview:uid=65532,gid=65532,mode=0700
    read_only: true
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
    # Shares the host's route to the review hosts, such as its tunnel.
    network_mode: host
    restart: unless-stopped

volumes:
  autoreview-state:
```

The secrets come from `./autoreview.env` (mode `0600`, outside `./config`) as
environment variables; see [secrets](knowledge/safety/secrets.md). Write the
configuration into `./config` with `init` from the image, then check it, as the
user that owns `./config`:

```sh
export AUTOREVIEW_VERSION=$version
mkdir -m 0755 config
docker run --rm --network none --user "$(id -u):$(id -g)" -v "$PWD/config:/etc/autoreview" \
  "ghcr.io/pohunekd/autoreview:$version" init --dir /etc/autoreview --login <login> \
  --pohunek-host <host> --example single-repo-comment
docker compose run --rm autoreview validate
docker compose up -d
```

## First configuration

Write the configuration, a policy from the [examples](../examples/README.md)
and its prompts with [`autoreview init`](cli.md#autoreview-init). It never
overwrites a file.

```sh
sudo autoreview init --dir /etc/autoreview --login <login> \
  --pohunek-host <host> --example single-repo-comment
```

The written `config.yaml` is the complete file of the
[configuration reference](configuration.md#complete-file), with the `login`
of the credential and `pohunek.hosts` set. Its paths are the ones of path b.
Set the homeserver, room and mentioned user of its placeholder `matrix` backend
in `notify` (see [notifiers](knowledge/guides/notifiers.md)), or, after 0.2.0,
pass them and the other paths with the optional
[layout flags of `init`](cli.md#autoreview-init).

The agent profile named as `reviewers.<id>.agent_profile` in `policy.yaml` must
exist on the review host. Install the same autoreview binary there and write the
profile with [`autoreview profiles install`](cli.md#autoreview-profiles-install),
run as the user of the pohunek daemon on that host. It also writes the reviewer
adapter that the profile starts, which needs Python 3.11 or newer as `python3`.

Check configuration, policy and prompts before every start, as the service user
with the secrets file of path b:

```sh
sudo -u autoreview autoreview --config /etc/autoreview/config.yaml \
  --env-file /etc/autoreview/autoreview.env validate
```

The output is one JSON object; error messages are listed in [policy validation errors](policy.md#validation-errors).

## First review

Start the service, then check what discovery picks up and run one pass. A review
starts after `schedule.debounce_seconds`.

```sh
run() { sudo -u autoreview autoreview --config /etc/autoreview/config.yaml \
  --env-file /etc/autoreview/autoreview.env "$@"; }
run candidates
run poll
run reviews
run review --repository <owner/repo> <number>
```

Open a test pull request in a matching repository and request a review from the
login of the credential, or add the `label:` trigger's label.

## Upgrade

Before you replace the binary or the image, read
[upgrade and backup](knowledge/runbooks/upgrade-and-backup.md).
