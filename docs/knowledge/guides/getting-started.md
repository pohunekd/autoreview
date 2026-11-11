---
type: Guide
id: guide/getting-started
title: Getting started
description: Set up autoreview on one Linux machine with scripts/setup - what autoreview does, what the script installs and where, each step and question, the second run after the policy edit, and what to do on each exit code.
source_kind: manual
intents: [setup, help]
---

# Getting started

> **Status: 0.2.0.** This page describes autoreview 0.2.0 and `scripts/setup`. The
> script installs the autoreview release named by `AUTOREVIEW_VERSION` in the
> script, which must know the `init` layout flags (after 0.2.0); parts marked
> planned are not in 0.2.0.

## What autoreview does

autoreview watches GitHub for pull requests that ask for a review, either by a
review request to its account or by a label. A policy decides which
repositories it reviews, with which reviewers and what it may do with the
result.

Each review runs coding agents (Claude Code or Codex) as read-only reviewers in
pohunek sessions on a review host. Their findings can be checked by a verifier,
and autoreview then publishes one GitHub review with inline comments: a
comment, a request for changes, an approval, or a wait for a person.

autoreview keeps its state in SQLite, so a restarted service resumes its
reviews, and it reports what needs attention to Matrix. The details are in
[how it works](../concepts/how-it-works.md).

## What the script sets up

`scripts/setup` sets up one Linux machine on which autoreview and the pohunek
daemon run together, as your own user, without `sudo`. Several machines are
described in [several review hosts](multihost.md#deployment-layout).

| What | Where |
|---|---|
| autoreview, from the release archive | `~/.local/bin/autoreview` |
| pohunek and its login service, from the release archive | `~/.local/bin/pohunek`, the systemd user unit `pohunek-<ns>-daemon.service` |
| The configuration, the policy and the prompts (`autoreview init`) | the directory you name, for example `~/autoreview` |
| The store | `autoreview.db` in that directory |
| The secrets file, empty, mode 0600 | `$XDG_CONFIG_HOME/autoreview/autoreview.env` (`~/.config/...`) |
| The official runtime packages and hooks of the agents | pohunek's own directories |
| The agent profiles and their adapters (`autoreview profiles install`) | `~/.config/pohunek/agents/`, `~/.local/share/autoreview/adapters/` |
| `autoreview.service`, `autoreview-queue.service` and `.timer` | `~/.config/systemd/user/` |
| Lingering of your user (`loginctl enable-linger`) | systemd-logind |

It does not install the agents themselves, `gh`, Python or system packages, and
it never overwrites a configuration or an agent profile that it did not write.

## Before you start

- Linux on `x86_64` with glibc or musl. autoreview also ships for `aarch64`,
  but pohunek publishes its daemon and runtime packages only for `x86_64`, so
  on `aarch64` the script stops unless pohunek and the packages are in place.
- systemd 255 or newer with a user manager and a session bus (on minimal
  Debian systems the `dbus-user-session` package), and `XDG_RUNTIME_DIR` set.
- `curl`, `tar`, `sha256sum`, `install`, `diff`, `stat`, `readlink`, `loginctl`,
  `systemd-run`, Python 3.11 or newer as `python3`, and `gh` logged in
  (`gh auth login`): the script verifies every download with
  `gh attestation verify`.
- Claude Code or Codex on your `PATH`, run once so that its config home
  (`~/.claude`, `~/.codex`) exists.
- A GitHub account for autoreview ([GitHub identity](../concepts/github-identity.md)),
  its token, and a Matrix room with an access token
  ([notifiers](notifiers.md)).
- Paths without spaces: the configuration directory, `HOME` and
  `XDG_CONFIG_HOME` may hold only letters, digits, `.`, `_`, `-` and `/`,
  because the unit files hold them as they are.
- Directories and files that only you can change. pohunek refuses a path when
  a directory on it, from `/` down, is a symbolic link, is group- or
  world-writable and yours, or belongs to another user (root's directories are
  fine when others cannot write them or they are sticky, as `/tmp` is), and
  when the directory or file at its end is not yours or is group- or
  world-writable. Systems that give each user a private group (Ubuntu, for
  example) set the umask `0002`, so directories that programs create there are
  group-writable. The detection step checks `$XDG_CONFIG_HOME`
  (`~/.config`), `$XDG_DATA_HOME` (`~/.local/share`), `$XDG_STATE_HOME`
  (`~/.local/state`), `~/.local/bin`, `~/.local/libexec/pohunek`, the
  directories below them that pohunek and the script use, and for Claude Code
  and Codex on your `PATH` their config home with its settings files. It lists
  every refused path at once, with `chmod go-w <path>` for a writable one, and
  stops with exit code 3; the script never changes a mode itself. One refusal
  of pohunek is not checked here and stops a later step with exit code 6: a
  private directory of pohunek itself (such as its state directory) whose mode
  is wider than pohunek requires; pohunek's error names the path.

## Run the script

From a checkout of the repository, in a terminal:

```sh
scripts/setup --dry-run --dir "$HOME/autoreview"   # only reads, prints the plan
scripts/setup --dir "$HOME/autoreview"
```

`--dry-run` detects and prints the plan and changes nothing; it asks nothing.
Without a terminal the script refuses to run unless `--dry-run` is given. The
answers can be given as flags: `--dir`, `--login`, `--pohunek-host`,
`--example`, `--matrix-homeserver-url`, `--matrix-room` and
`--matrix-mention-user`. A missing answer is asked; an empty answer stops the
script. There is no `--yes`.

Before each step that changes the machine, the script prints the exact
commands and asks `Proceed? [y/N]`. Only `y` or `yes` goes on; any other
answer stops the script before that step, with nothing of the step changed. A
step that is in place is skipped without a question.

## The steps

1. **Detection.** The platform, the tools, systemd, `gh auth status`, the
   paths, and the paths that pohunek refuses
   ([before you start](#before-you-start)). It only reads.
2. **autoreview.** Downloads the archive of `AUTOREVIEW_VERSION` and its
   `.sha256`, checks the checksum and the attestation of the release workflow
   (as [install](../../install.md#a-release-binary) does) and installs the
   binary. An installed newer release is kept; nothing is downgraded.
3. **pohunek.** When pohunek is missing or older than `POHUNEK_MIN_VERSION`,
   downloads and verifies the daemon archive of `POHUNEK_VERSION` and runs its
   `packaging/install-daemon.sh`, which installs or upgrades the login service.
   A newer pohunek is kept.
4. **Configuration.** When `<dir>/config.yaml` does not exist, asks the GitHub
   login, the pohunek host name of this machine (`local` names it as well; any
   other name must resolve to the same `host_id`), the
   [example](../../../examples/README.md) to start from and the three Matrix
   values, then runs `autoreview init` with them and with the paths of this
   machine (`--store-path`, `--gh-bin`, `--pohunek-bin`,
   `--pohunek-state-home`, `--pohunek-runtime-dir`). An existing configuration
   is never overwritten.
5. **Secrets file.** Creates the empty secrets file with mode 0600 when it is
   missing.

The run that created the configuration stops here: every example names
repositories of other people. Edit `<dir>/policy.yaml` and set the
repositories you want reviewed, then write each secret as a `NAME=value` line
into the secrets file with an editor (`AUTOREVIEW_GH_TOKEN` and
`AUTOREVIEW_MATRIX_TOKEN` in every example). Never type a token on a command
line. Then run the script again with the same `--dir`, as the stop message
prints it; it goes on with:

6. **Secrets and validate.** Reads the variable names of the `*_env` keys of
   `config.yaml` and prints each as `set` or `missing` in the secrets file; it
   never reads, prints or writes a value. Then it runs
   `autoreview validate` in a transient user unit with the secrets file, the
   environment the service gets, and reads the agent profiles the policy needs.
7. **Profile questions.** For each profile that does not exist yet: the agent
   (`claude` for `claude-structured` reviewers, `codex` for `codex-envelope`
   ones, as `reviewers.<id>.protocol` in the policy says), the model, the
   effort and the largest request in bytes; for Codex also the largest report
   (at most `limits.result_max_bytes`) and the time the adapter stays after
   its report (at least `session.timeout_seconds`). The agent executable is the
   one on your `PATH`. A profile written by `autoreview profiles install` is
   kept and asks nothing.
8. **Runtime packages.** For each agent without its official pohunek package,
   `pohunek plugin install runtime/packages/<agent>.tar.zst --catalog
   runtime/runtime-catalog.json --yes` from the daemon archive of the running
   pohunek release, downloaded and verified as in step 3.
9. **Hooks.** `pohunek integration install --agent <agent>`, asked on its own
   because pohunek has no dry run of it.
10. **Agent profiles.** `autoreview profiles install --host local` with the
    answers of step 7 ([CLI](../../cli.md#autoreview-profiles-install)).
11. **User service.** Prints the `match` and `trigger` lines of the policy,
    writes `autoreview.service` (`serve`), `autoreview-queue.service` and
    `autoreview-queue.timer` (hourly, 08:00 to 20:00) with the secrets file as
    their `EnvironmentFile`, and runs `systemctl --user daemon-reload` and
    `enable --now`. A unit file with other text is shown as a diff and replaced
    only after its own question. When the running service does not run the
    installed autoreview binary (this run or an earlier one replaced it), it is
    restarted, after a question.
12. **Lingering.** Without it, systemd stops pohunek and autoreview when you log
    out and starts them only at the next login.

A third run with the same `--dir` on the finished setup prints every step as
`in place, skipped`, asks nothing and ends with `setup: nothing to change`.

## Exit codes

| Code | Meaning | What to do |
|---|---|---|
| 0 | Done, nothing to change, a dry run, or the stop after the configuration was written | After that stop: edit the policy, fill the secrets file, run the script again |
| 1 | A bad flag or an empty answer | Run again with the flag or the answer |
| 2 | A platform the release archives do not cover | Install pohunek by hand; the script supports Linux `x86_64` |
| 3 | A missing tool or condition, or a path that pohunek refuses ([before you start](#before-you-start)) | Do what the message names after `;` and each listed path names (`chmod go-w <path>` for a writable one), then run again. The hooks step stops the same way, with pohunek's reason, when pohunek refuses the config home it uses for an agent |
| 4 | You declined a step, or stdin is not a terminal | Nothing of that step changed; run again in a terminal |
| 5 | A download, its checksum or its attestation failed | Check the network and the release; never install an archive that failed |
| 6 | A pohunek command failed | Read pohunek's message above it; `pohunek doctor` and the pohunek install guide help. When its installer lists live sessions that an upgrade would not carry over, end them (`pohunek session list` shows them) and run the script again, or download and verify the daemon archive by hand as the pohunek install guide describes and run its `packaging/install-daemon.sh --accept-runtime-loss`; the temporary copy that its message names is removed when the script exits |
| 7 | Installing autoreview failed | Check that `~/.local/bin` is writable |
| 8 | `autoreview init` or `profiles install` refused | Look up the code in the [CLI error codes](../../cli.md#error-codes) |
| 9 | A secret is missing, the secrets file is not mode 0600, or `validate` failed | Fill the file with an editor, or fix the key that the code names |
| 10 | Writing the units, `systemctl --user` or `loginctl` failed | `journalctl --user -u autoreview.service` shows the service log |

## After the setup

- The service reads the configuration and the policy at start: after an edit,
  run `autoreview --config <dir>/config.yaml validate` with the secrets set and
  `systemctl --user restart autoreview.service`.
- Register each repository of the policy with `pohunek project add` and run a
  first review as the [quickstart](quickstart.md) describes from step 9 on.
- After an upgrade of autoreview, run `autoreview profiles install` with the
  same flags for each profile, and `--pohunek-timeout-seconds`, which 0.3.0 and
  earlier did not take; the script keeps a profile it wrote and does not know
  the flags of an earlier run.
- Before you leave it running, read the
  [threat model](../safety/threat-model.md).
