---
type: Guide
id: guide/quickstart
title: Quickstart
description: From nothing to a first autoreview comment on one repository - prerequisites, install, the single-repo-comment example, agent profiles, a validated config, one manual review and the service.
source_kind: manual
intents: [setup, help]
---

# Quickstart

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

This guide takes one repository from nothing to a review comment posted by
autoreview. It uses one reviewer and comments only, so nothing is approved
or blocked. Once it works, pick another scenario from the
[use cases](index.md).

The steps assume a Linux host. The review itself runs on a pohunek host,
which can be the same machine.

## 1. Check the prerequisites

- A pohunek daemon running on the review host, with the official Codex
  runtime package installed. The reviewer in this guide uses Codex. See the
  [pohunek project](https://github.com/zajca/pohunek) for installation.
- The repository you want reviewed, registered as a pohunek project on that
  host with `pohunek project add`. autoreview finds the repository by its
  origin URL, so the registered checkout must point at the same remote.
- The GitHub CLI (`gh`), installed and able to run on the autoreview host.
- A GitHub account for autoreview. It can be your own account or a machine
  user. Reviews are posted from this account, so read
  [GitHub identity](../concepts/github-identity.md) before you choose.

## 2. Install the binary

Install the `autoreview` binary from the release page of
[pohunekd/autoreview](https://github.com/pohunekd/autoreview). Confirm it
runs:

```sh
autoreview --help
```

A machine that already runs pohunek as the owner of the review can install autoreview as a
[pohunek workflow plugin](../../install.md#d-pohunek-workflow-plugin) instead; the rest of this guide then
runs its commands as `pohunek workflow run pohunekd.autoreview <action>`.

## 3. Create a working directory

Pick a directory for the configuration. The rest of this guide calls it
`$AUTOREVIEW_HOME`.

```sh
export AUTOREVIEW_HOME="$HOME/autoreview"
```

## 4. Write the starting files

`autoreview init` writes `config.yaml`, the `policy.yaml` of the example, and
the two prompts. It refuses to overwrite files, so run it once in an empty
directory. Use your own GitHub login and the pohunek host name from step 1.

```sh
autoreview init --dir "$AUTOREVIEW_HOME" \
  --login <github-login> \
  --pohunek-host <host> \
  --example single-repo-comment
```

The command prints the agent profiles the policy needs. Keep that list for
step 6.

## 5. Point the policy at your repository

Open `$AUTOREVIEW_HOME/policy.yaml`. In the `repositories` list, set `match`
to the repository you want reviewed, in lowercase `owner/repository` form.
Leave the rest of the example unchanged for this guide. The `trigger` of the
example is `label:ai:review`, so a review starts when a pull request carries
the `ai:review` label. Create that label in the repository if it does not
exist yet.

The configuration `init` writes routes every notification level to the `none`
backend, so nothing reaches a person yet; the results appear on GitHub and in
`autoreview reviews`. Add a notifier later with the
[notifiers guide](notifiers.md).

## 6. Install the agent profile

The policy names one agent profile for its reviewer. Create it on the review
host with the profile name from the `init` output:

```sh
autoreview profiles install \
  --host <host> \
  --agent codex \
  --model <model> \
  --name <profile-name>
```

The base runtime must already be installed on that host, see step 1.
Running the command again with the same arguments changes nothing.

## 7. Provide the GitHub token

`init` wrote one credential that covers every repository and reads its token from
`AUTOREVIEW_GH_TOKEN`. Export the token from your secret store into the shell that
runs autoreview. Do not print it, paste it into a file, or put it in the
configuration. To check that it is set without showing it:

```sh
test -n "$AUTOREVIEW_GH_TOKEN" && echo set
```

autoreview gives this token only to the `gh` calls for its repositories. Before
the first run, the token must belong to the `login` of that credential in
`config.yaml`, the value you passed to `init`. The service refuses to start
otherwise (`credential_login_mismatch`).

## 8. Validate the configuration

Point autoreview at the configuration and check it. `validate` loads the
config and the policy, reads every prompt, and confirms that the pohunek CLI
answers.

```sh
export AUTOREVIEW_CONFIG="$AUTOREVIEW_HOME/config.yaml"
autoreview validate
```

A valid setup prints one JSON object with `policy_digest`,
`agent_profiles` and `pohunek_version`. An error prints `error` with a code
and the name of the key that failed. Fix it and run `validate` again; see
[configuration](../../configuration.md) for every key.

## 9. Check what would be reviewed

`candidates` is read-only. It prints the pull requests the next discovery
pass would consider, per rule and trigger.

```sh
autoreview candidates
```

If your repository is missing, no open pull request carries the label yet,
or the rule does not match the repository name.

## 10. Label a pull request

On GitHub, open a pull request in the repository and add the `ai:review`
label. Use a small pull request for the first run. A pull request with a
normal diff is more useful than an empty one, because the reviewer needs
changes to check.

## 11. Run one discovery pass

The service looks for new requests every minute. To start immediately, run
one pass by hand. It is safe while `serve` runs, because passes take turns on
a lock.

```sh
autoreview poll
```

The output lists what the pass queued. The review starts shortly after,
once the debounce period of `schedule.debounce_seconds` has passed.

## 12. Watch the review

List the newest reviews, then inspect the one for your pull request:

```sh
autoreview reviews
autoreview review --repository <owner>/<repo> <pr-number>
```

`review` shows the current head, the state of each reviewer, and the
decision once it is made. A review that failed shows the failure code, which
you can look up in the [CLI error codes](../../cli.md#error-codes).

## 13. See the comment on GitHub

When the review is complete, autoreview publishes one GitHub review on the
pull request. In this example the outcome is a comment: the body contains the
summary, the findings as inline comments, and the AI signature as its last
line. A pull request you open yourself gets a comment in any case, because
GitHub does not allow an author to approve their own pull request.

## 14. Repeat with a second push

Push a new commit to the pull request. After the debounce period, the review
runs again. The reviewer of this example is Codex, and it re-reviews only the
new commits and settles earlier findings, as a Claude reviewer does. See
[incremental reviews](../concepts/incremental-reviews.md).

## 15. Run as a service

Once one review works, run autoreview as a long-running service so it reviews
without manual polls. Install the systemd unit or the container image as
described in [install](../../install.md), and let `autoreview serve` run until
you stop it. Reviews that are running at shutdown resume after the next
start.

Before you leave this setup running, read the
[threat model](../safety/threat-model.md). Repositories with untrusted
authors need the rules from [public repositories](public-repositories.md).

## Next steps

- Add a notifier so you learn when a review needs you:
  [notifiers](notifiers.md).
- Add an approval step: [human approval](human-approval.md).
- Run reviewers on a second host: [multihost](multihost.md).
