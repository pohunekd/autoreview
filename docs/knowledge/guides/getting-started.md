---
type: Guide
id: guide/getting-started
title: Getting started
description: Set up autoreview on one Linux machine or one Mac with Apple Silicon with scripts/setup - what autoreview does, what the script installs and where, each step and question, the second run after the policy edit, and what to do on each exit code.
source_kind: manual
intents: [setup, help]
---

# Getting started

> **Status: after 0.6.0.** This page describes autoreview on `main` after the
> 0.6.0 release and `scripts/setup`. The script installs the autoreview release
> named by `AUTOREVIEW_VERSION` in the script, which must know the `init` layout
> flags and `profiles install --pohunek-timeout-seconds` (0.4.0 or later). A
> part that is not available names its open issue. On a Mac the script needs a
> release with an `aarch64-apple-darwin` archive, 0.6.0 or later (#513).

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

`scripts/setup` sets up one Linux machine, or one Mac with Apple Silicon, on
which autoreview and the pohunek daemon run together, as your own user, without
`sudo`. The table is for Linux; [on a Mac](#on-a-mac) the service parts differ.
Several machines are described in [several review hosts](multihost.md#deployment-layout). To set up
each step by hand, or on a system without systemd user units, follow the
[quickstart](quickstart.md); for a dedicated server user, see
[install](../../install.md).

| What | Where |
|---|---|
| autoreview, from the release archive | `~/.local/bin/autoreview` |
| pohunek and its login service, from the release archive | `~/.local/bin/pohunek`, the systemd user unit `pohunek-<ns>-daemon.service` |
| The configuration, the policy and the prompts (`autoreview init`, run under `umask 077`) | the directory you name, for example `~/autoreview`, mode 0700 |
| The store, private to your user (the units set `UMask=0077`) | `autoreview.db` in that directory |
| The secrets file, empty, mode 0600 | `$XDG_CONFIG_HOME/autoreview/autoreview.env` (`~/.config/...`) |
| The official runtime packages and hooks of the agents | pohunek's own directories |
| The agent profiles and their adapters (`autoreview profiles install`) | `~/.config/pohunek/agents/`, `~/.local/share/autoreview/adapters/` |
| `autoreview.service`, `autoreview-queue.service` and `.timer` | `~/.config/systemd/user/` |
| Lingering of your user (`loginctl enable-linger`) | systemd-logind |

It does not install the agents themselves, `gh`, Python or system packages, and
it never overwrites a configuration or an agent profile that it did not write.

### On a Mac

| What | Where |
|---|---|
| pohunek and its login agent | `~/.local/bin/pohunek`, `~/Library/LaunchAgents/io.github.zajca.pohunek.<ns>.daemon.plist` |
| The review service and the hourly queue (08:00 to 20:00), as launchd agents of your login session | `~/Library/LaunchAgents/io.github.pohunekd.autoreview.plist` and `io.github.pohunekd.autoreview-queue.plist` |
| Their logs | `$XDG_STATE_HOME/autoreview/logs/autoreview.log` and `autoreview-queue.log` (`~/.local/state/...`), directory mode 0700 |

The rest is as in the table above, without the units and without lingering. The
agents pass the secrets file with `--env-file` and carry no secret: only `PATH`
(the search path that pohunek recorded for its daemon), the XDG homes and, when
it is set, `XDG_RUNTIME_DIR`. They restart after a failure, as the units do, and
they run while you are logged in: launchd starts them at login and stops them at
logout, as it does the pohunek daemon, so a Mac that reviews unattended needs
automatic login. launchd does not rotate the logs; see
[logs](../runbooks/troubleshooting.md#logs).

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
- `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_STATE_HOME` unset, for
  `~/.config`, `~/.local/share` and `~/.local/state`, or set to an absolute
  path: pohunek refuses one that is set but empty, so the detection step stops
  on it with exit code 3.
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

On a Mac, the Linux items about systemd, `sha256sum`, `loginctl` and
`systemd-run` do not apply; instead:

- macOS 14 or newer on Apple Silicon (`arm64`).
- bash 4 or newer: macOS runs its own bash 3.2 as `bash`, and the script stops
  with exit code 3 on it. Install it with `brew install bash` and run the script
  with it: `"$(brew --prefix)/bin/bash" scripts/setup ...`.
- `shasum`, `curl`, `tar`, `install`, `diff`, `stat` and `readlink` (macOS ships
  them), Python 3.11 or newer as `python3` and `gh` logged in, for example from
  Homebrew.
- Python 3.11 or newer also on the `PATH` of the pohunek daemon: launchd gives
  it the search path that `pohunek service install` recorded, not the `PATH` of
  your shell, and the reviewer adapters start with the first `python3` there.
  After a new Python, run `pohunek service refresh-path`.
- `XDG_RUNTIME_DIR` unset, or set to the absolute path it had when pohunek's
  service was installed: pohunek gives its daemon that value, and the script
  passes it on to the configuration and the agents. Unset, pohunek uses its
  macOS default `/private/tmp/pohunek-<uid>`.
- The configuration directory and the XDG homes outside `~/Desktop`,
  `~/Documents`, `~/Downloads` and iCloud Drive: macOS privacy protection
  closes them to launchd jobs, so the script refuses them.
- Claude Code as the agent: the Codex adapter needs Linux, and
  `autoreview profiles install --agent codex` refuses on a Mac with
  `profile_agent_unsupported`, so the script stops with exit code 8 at a Codex
  profile. Codex reviewers run on a Linux review host
  ([several review hosts](multihost.md)).
- A terminal in your logged-in session (not a plain SSH login): launchd loads
  the agents into the login session.

## Run the script

From a checkout of the repository, in a terminal:

```sh
scripts/setup --dry-run --dir "$HOME/autoreview"   # only reads, prints the plan
scripts/setup --dir "$HOME/autoreview"
```

`--dry-run` detects and prints the plan and changes nothing; it asks nothing.
Without a terminal the script refuses to run unless `--dry-run` is given. The
answers can be given as flags: `--dir`, `--login`, `--pohunek-host`,
`--example`, `--matrix-homeserver-url`, `--matrix-room`,
`--matrix-mention-user` and `--pohunek-timeout-seconds`. A missing answer is
asked; an empty answer stops the script. There is no `--yes`.

Before each step that changes the machine, the script prints the exact
commands and asks `Proceed? [y/N]`. Only `y` or `yes` goes on; any other
answer stops the script before that step, with nothing of the step changed. A
step that is in place is skipped without a question.

## The steps

1. **Detection.** The platform, the tools, systemd (on Linux), `gh auth
   status`, the paths, and the paths that pohunek refuses
   ([before you start](#before-you-start)). It only reads.
2. **autoreview.** Downloads the archive of `AUTOREVIEW_VERSION` and its
   `.sha256`, checks the checksum and the attestation of the release workflow
   (as [install](../../install.md#a-release-binary) does) and installs the
   binary. An installed newer release is kept; nothing is downgraded.
3. **pohunek.** When pohunek is missing or older than `POHUNEK_MIN_VERSION`,
   downloads and verifies the daemon archive of `POHUNEK_VERSION` and runs its
   `packaging/install-daemon.sh`, which installs or upgrades the login service.
   A newer pohunek is kept. On a Mac it then checks that the daemon's recorded
   search path holds Python 3.11 or newer.
4. **Configuration.** When `<dir>/config.yaml` does not exist, asks the GitHub
   login, the pohunek host name of this machine (`local` names it as well; any
   other name must resolve to the same `host_id`), the
   [example](../../../examples/README.md) to start from and the three Matrix
   values, then runs `autoreview init` with them and with the paths of this
   machine (`--store-path`, `--gh-bin`, `--pohunek-bin`,
   `--pohunek-state-home`, `--pohunek-runtime-dir`; on a Mac the last one only
   when `XDG_RUNTIME_DIR` is set). An existing configuration is never
   overwritten.
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
   `autoreview validate`: on Linux in a transient user unit with the secrets
   file, the environment the service gets; on a Mac directly, in the
   environment of the script, with `--env-file`. It reads the agent profiles
   the policy needs from the output.
7. **Profile questions.** For each profile that does not exist yet: the agent
   (`claude` for `claude-structured` reviewers, `codex` for `codex-envelope`
   ones, as `reviewers.<id>.protocol` in the policy says), the model, the
   effort and the largest request in bytes; for Codex also the largest report
   (at most `limits.result_max_bytes`) and the time the adapter stays after
   its report (at least `session.timeout_seconds`). The agent executable is the
   one on your `PATH`. A profile written by `autoreview profiles install` is
   kept and asks nothing. When any profile is missing, the script then asks
   once for the seconds after which `profiles install` kills a pohunek call;
   give the `pohunek.timeout_seconds` of `config.yaml`. It has no default: a
   value that is not a whole number above 0 stops the script with exit code 1.
8. **Runtime packages.** For each agent without its official pohunek package,
   `pohunek plugin install runtime/packages/<agent>.tar.zst --catalog
   runtime/runtime-catalog.json --yes` from the daemon archive of the running
   pohunek release, downloaded and verified as in step 3.
9. **Hooks.** `pohunek integration install --agent <agent>`, asked on its own
   because pohunek has no dry run of it.
10. **Agent profiles.** `autoreview profiles install --host local` with the
    answers of step 7, including `--pohunek-timeout-seconds`
    ([CLI](../../cli.md#autoreview-profiles-install)).
11. **User service.** Prints the `match` and `trigger` lines of the policy,
    writes `autoreview.service` (`serve`), `autoreview-queue.service` and
    `autoreview-queue.timer` (hourly, 08:00 to 20:00) with the secrets file as
    their `EnvironmentFile`, and runs `systemctl --user daemon-reload`, `enable`
    and one `restart` of the service and the timer, so the service starts once.
    A unit file with other text is shown as a diff and replaced
    only after its own question. When the running service does not run the
    installed autoreview binary (this run or an earlier one replaced it), it is
    restarted, after a question.
12. **Lingering.** Without it, systemd stops pohunek and autoreview when you log
    out and starts them only at the next login.

On a Mac, steps 11 and 12 are one step, **launchd agents**. When both agent
files hold the text of this run, the review agent runs and the queue agent is
loaded, the step changes nothing, except after an install of autoreview or when
the running service started before the installed binary changed: then it asks
to restart the service and runs
`launchctl kickstart -k gui/<uid>/io.github.pohunekd.autoreview`, and ends
there. Otherwise it prints the `match` and `trigger` lines, then installs the
two agents one after the other, which also starts the service on the installed
binary. For each, it runs `launchctl bootout gui/<uid>/<label>` when the agent is
loaded and waits until launchd no longer lists it, then writes the file and
runs `launchctl enable` and `launchctl bootstrap gui/<uid> <file>`. A stopping
job can take the agent's stop budget of 90 s, so the wait ends after 95 s; a
bootout that exits 36 (the job is still stopping) counts as done. An
agent file with other text is shown as a diff and replaced only after its own
question. Each failure stops with exit code 10 and leaves that agent as follows:
- the previous agent file cannot be read: the script stops before it changes
  that agent;
- a failed bootout: its file and job are unchanged;
- a job still listed after 95 s: the file is unchanged, and the job is not
  loaded again when it stops; run the script again then;
- writing, enabling or loading the new definition failed: the previous file is
  loaded, written back first when it changed, or the new file is removed when
  there was none;
- writing the previous file back or loading it failed too: the job is not
  loaded; fix what the message names and run the script again.

An agent installed earlier in the same run stays installed. After the install,
the step prints the [newsyslog lines](../runbooks/troubleshooting.md#logs) for
the logs and the commands that remove the agents:
`launchctl bootout gui/<uid>/<label>` and deleting
`~/Library/LaunchAgents/<label>.plist`, for both labels. Then check them with
`scripts/macos-check service`. It reads only, prints an `ok` line for each
check and stops at the first `FAIL` with exit code 1:
- the platform: Darwin arm64, macOS 14 or newer;
- both agents pass `plutil -lint`, are loaded and hold no secret;
- the review agent runs, and its process runs the binary at the agent's
  program path (links resolved). It does not show whether that process started
  before the file was replaced: a run of `scripts/setup` asks to restart a
  service that started before the installed binary changed;
- the queue has its 13 hours;
- the secrets file is yours with mode 0600;
- the log is private;
- the pohunek socket the service dials, consistent with `pohunek.runtime_dir`;
- one `autoreview validate` in the agent's environment.

A third run with the same `--dir` on the finished setup prints every step as
`in place, skipped`, asks nothing and ends with `setup: nothing to change`.

## Exit codes

| Code | Meaning | What to do |
|---|---|---|
| 0 | Done, nothing to change, a dry run, or the stop after the configuration was written | After that stop: edit the policy, fill the secrets file, run the script again |
| 1 | A bad flag or an empty answer; on a Mac also a `--dir` in a folder that privacy protection closes | Run again with the flag or the answer, or with a `--dir` outside those folders |
| 2 | A platform the release archives do not cover | Install pohunek by hand; the script supports Linux `x86_64`, and macOS 14 or newer on Apple Silicon |
| 3 | A missing tool or condition, or a path that pohunek refuses ([before you start](#before-you-start)) | Do what the message names after `;` and each listed path names (`chmod go-w <path>` for a writable one), then run again. The hooks step stops the same way, with pohunek's reason, when pohunek refuses the config home it uses for an agent. On a Mac also bash 3.2, a relative `XDG_RUNTIME_DIR`, an XDG home in a folder that privacy protection closes, or no Python 3.11 on the daemon's search path (`pohunek service refresh-path`) |
| 4 | You declined a step, or stdin is not a terminal | Nothing of that step changed; run again in a terminal |
| 5 | A download, its checksum or its attestation failed | Check the network and the release; never install an archive that failed |
| 6 | A pohunek command failed | Read pohunek's message above it; `pohunek doctor` and the pohunek install guide help. When its installer lists live sessions that an upgrade would not carry over, end them (`pohunek session list` shows them) and run the script again, or download and verify the daemon archive by hand as the pohunek install guide describes and run its `packaging/install-daemon.sh --accept-runtime-loss`; the temporary copy that its message names is removed when the script exits |
| 7 | Installing autoreview failed | Check that `~/.local/bin` is writable |
| 8 | `autoreview init` or `profiles install` refused | Look up the code in the [CLI error codes](../../cli.md#error-codes) |
| 9 | A secret is missing, the secrets file is not mode 0600, or `validate` failed | Fill the file with an editor, or fix the key that the code names |
| 10 | Writing the units, `systemctl --user` or `loginctl` failed; on a Mac, an agent file could not be read (that agent unchanged), writing an agent, creating its directories or `launchctl` failed, a booted-out agent was still listed after 95 s, or restoring the previous agent failed (its job not loaded) | `journalctl --user -u autoreview.service` shows the service log; on a Mac it is `~/.local/state/autoreview/logs/autoreview.log`, and `launchctl print gui/$(id -u)/io.github.pohunekd.autoreview` shows the agent. The message names that agent and what it left ([launchd agents](#the-steps)); an agent installed earlier in the run stays installed. A `launchctl bootstrap` from an SSH login without a logged-in session fails; run the script in your session |

## After the setup

- The service reads the configuration and the policy at start: after an edit,
  run `autoreview --config <dir>/config.yaml validate` with the secrets set and
  `systemctl --user restart autoreview.service`; on a Mac
  `launchctl kickstart -k gui/$(id -u)/io.github.pohunekd.autoreview`.
- Register each repository of the policy with `pohunek project add` and run a
  first review as the [quickstart](quickstart.md) describes from step 9 on. A
  reviewer on a `checkout: base` pipeline (the examples use it) reads the change
  through git, so the pull request head must be in that project repository on
  the review host. With pohunek 0.43.0 or later, both the CLI and the review
  host's daemon, the session start fetches it. With an older pohunek, run
  `git -C <project repository> fetch origin refs/pull/<n>/head` there before
  the review, or the review stops with `review_head_unavailable`; see
  [Head commit unavailable](../runbooks/troubleshooting.md#head-commit-unavailable).
- After an upgrade of autoreview, run `autoreview profiles install` with the
  same flags for each profile, and `--pohunek-timeout-seconds`, which 0.3.0 and
  earlier did not take; the script keeps a profile it wrote and does not know
  the flags of an earlier run. When it upgrades an autoreview older than 0.4.0,
  whose Codex adapters 0.4.0 refuses (`review_nonce_mismatch`), it names each
  Codex profile it kept and prints the command with placeholders for the flags.
- A directory made by an earlier setup run may have mode 0755 and its store
  0644, which lets other local users read the review history. From 0.5.0 on,
  `validate`, and with it the script, stops on such a store
  with exit code 9 and prints the `chmod` command for each path. Run
  `chmod 700 <dir>` and `chmod 600 <dir>/autoreview.db*`, with the `--dir` you
  gave the script, before the upgrade or when it stops, then run the script
  again.
- Before you leave it running, read the
  [threat model](../safety/threat-model.md).
