---
type: Reference
id: reference/install
title: Install autoreview
description: Prerequisites, the two install paths (release archive with checksum and attestation, systemd units with their installer), building from source, first configuration, the first review and where to go for upgrades.
source_kind: manual
intents: [setup, help]
---

# Install autoreview

> **Status: after 0.4.0.** This page describes autoreview on `main` after the
> 0.4.0 release. A part that is not available names its open issue. The release
> archives are published from `v0.1.0` on; the release workflow builds and
> attests them. The systemd units and their installer of path b are in the
> archives from 0.4.0 on. A container image is not available (#40).

autoreview runs as one service with the GitHub CLI and the `pohunek` CLI. The
agents run as reviewers in pohunek sessions on the **review hosts**, where a
pohunek daemon runs and the repositories are registered.

## Prerequisites

- **pohunek daemon on each review host**, and the `pohunek` CLI at `pohunek.bin`
  on the autoreview host, in the supported range: 0.41.0 or later, before 1.0.0.
  A `checkout: head` pipeline needs 0.43.1 or later on both sides; with an older
  one each such review fails with `review_host_repo_hooks_unsupported`.
  `validate` and `serve` run `pohunek --version` and refuse a release outside
  the range with `pohunek_version_unsupported` and an output they cannot read
  with `pohunek_version_invalid`, see [validate](cli.md#autoreview-validate).
  The engine runs these CLI commands, each after `--host <host>` or naming the
  host: `project list`, `session list`, `session new`, `session output` and
  `session rm`, and `host command run` with a
  [`host_state`](configuration.md#pohunek) section.
  The check reads the CLI's version only; the daemon on each review host is
  reached through that CLI.
- **Runtime packages** `claude` and/or `codex`, installed on each review host per the
  [pohunek runtime packages guide](https://pohunek.dev/docs/guides/runtime-packages/).
- **Repositories registered** with `pohunek project add` on a review host. The first
  host of `pohunek.hosts` that has the repository runs its reviews, unless
  `pohunek.route_overrides` pins it ([multiple hosts](knowledge/guides/multihost.md)).
- **GitHub CLI (`gh`)** on the autoreview host.
- **GitHub token** of the account autoreview acts as, held in the variable named
  by the `token_env` of the one credential (`AUTOREVIEW_GH_TOKEN` in the
  example). It belongs to the credential's `login`; a token per owner or
  repository is one more credential with its own `token_env`.
  A personal token or a machine user works; see [credentials](configuration.md#credentials)
  and [GitHub identity](knowledge/concepts/github-identity.md).
- **Git access on each review host.** The review hosts fetch the code with their own
  git credentials, not with these tokens, so a private repository must also be
  reachable from the review host.

## Choose an install path

To run autoreview and the pohunek daemon together on one Linux machine as your
own user, run `scripts/setup` from a checkout of the repository: it installs
both from their release archives, writes the configuration and the user
service, and asks before every change. See
[getting started](knowledge/guides/getting-started.md). The paths below are for
a dedicated server user. To learn the steps on one repository first, follow the
[quickstart](knowledge/guides/quickstart.md). To let a coding agent take these
steps with you, paste the prompt of
[install with an agent](../README.md#install-with-an-agent) into it.

| Path | Use it when | You get |
|---|---|---|
| a. Release binary (default) | You run the service yourself under a dedicated server user | `autoreview` in `/usr/local/bin` |
| b. systemd units | You want the service and the hourly queue as units under a dedicated server user | `deploy/install.sh`, which installs `autoreview.service`, `autoreview-queue.service`, `autoreview-queue.timer` |

Paths a and b need the `autoreview` system user. The installer of path b
creates it; with path a only, create it yourself. A container image is not
available (#40).

## a. Release binary

The release archive is the default install path. Each release of
`pohunekd/autoreview` carries a static Linux binary per target,
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, as
`autoreview-<version>-<target>.tar.gz`, with its `.sha256` file and a build
provenance attestation. The binary needs no system library.

Download the archive and its checksum file, then check the checksum:

```sh
version=0.4.0                        # the release to install, without the v
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
`rustup toolchain install` without arguments (rustup 1.28 or later) installs
the toolchain of `rust-toolchain.toml`.

```sh
git clone --branch "v$version" https://github.com/pohunekd/autoreview
cd autoreview
rustup toolchain install
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

From 0.4.0 on, the release archive of path a also holds `deploy/install.sh` and
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
stops and the running service stays as it was. `validate` runs the `pohunek`
CLI at `pohunek.bin`, so install a supported pohunek first. Then it enables `autoreview.service` and
`autoreview-queue.timer`, and restarts the service when the binary or a unit
changed or when it is not running. Its exit codes: 1 usage, 2 not root or not
an autoreview binary, 3 the secrets file, 4 `validate`, 5 a failed install
step.

With `--root DIR` the installer places every path under `DIR` and skips the
steps that need root: the user, the ownership, `validate` and `systemctl`.
`DIR` and the directories under it that hold the installed paths must belong to
the caller or root, and neither group nor others may write to them.

The secrets file holds the variable of the credential's `token_env` and the
backend secrets as `NAME=value` lines. Write the values with an editor, never
on a command line, for example with `sudoedit /etc/autoreview/autoreview.env`.
See [secrets](knowledge/safety/secrets.md). The units pass the file with
`--env-file`, so it must be owned by `autoreview` with mode `0600` or `0400`; the
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
in `notify` (see [notifiers](knowledge/guides/notifiers.md)), or pass them and
the other paths with the optional
[layout flags of `init`](cli.md#autoreview-init).

The agent profile named as `reviewers.<id>.agent_profile` in `policy.yaml` must
exist on the review host. Install the same autoreview binary there and write the
profile with [`autoreview profiles install`](cli.md#autoreview-profiles-install),
run as the user of the pohunek daemon on that host. It also writes the reviewer
adapter that the profile starts, which needs Python 3.11 or newer as `python3`.
The Codex adapter also needs Linux (`PR_SET_CHILD_SUBREAPER`); on another system
it exits with status 64 before Codex starts.

Check configuration, policy and prompts before every start, as the service user
with the secrets file of path b:

```sh
sudo -u autoreview autoreview --config /etc/autoreview/config.yaml \
  --env-file /etc/autoreview/autoreview.env validate
```

The output is one JSON object; error messages are listed in [policy validation errors](policy.md#validation-errors).

## First review

With path b, the installer has started `autoreview.service` once the
configuration exists. With path a, start `autoreview serve` as the `autoreview`
user with the same `--config` and `--env-file` as below. Then check what
discovery picks up and run one pass. A review starts after
`schedule.debounce_seconds`.

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
