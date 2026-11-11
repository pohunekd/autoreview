---
type: Guide
id: guide/quickstart
title: Quickstart
description: From nothing to a first autoreview comment on one repository - prerequisites, install, the single-repo-comment example, agent profiles, a validated config, one manual review and the service.
source_kind: manual
intents: [setup, help]
---

# Quickstart

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0. The container image this page
> mentions is not in 0.2.0 and is planned in #40.

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

On one Linux machine that also runs the pohunek daemon,
[`scripts/setup`](getting-started.md) (after 0.2.0) does steps 2 to 8 of this guide and
installs the service; continue with step 9 after it. By hand: install the
`autoreview` binary from the release page of
[pohunekd/autoreview](https://github.com/pohunekd/autoreview). Confirm it
runs:

```sh
autoreview --help
```

## 3. Create a working directory

Pick a directory for the configuration. The rest of this guide calls it
`$AUTOREVIEW_HOME`.

```sh
export AUTOREVIEW_HOME="$HOME/autoreview"
```

## 4. Write the starting files

Write the configuration, the policy of the example and its prompts with
[`autoreview init`](../../cli.md#autoreview-init). Use your GitHub login and
the host name from step 1:

```sh
autoreview init --dir "$AUTOREVIEW_HOME" --login <your-login> \
  --pohunek-host <host> --example single-repo-comment
```

`init` sets `policy_file`, the `login` of the credential and `pohunek.hosts`. In
`$AUTOREVIEW_HOME/config.yaml`, set `store.path` to a file under
`$AUTOREVIEW_HOME`. After 0.2.0, `init` takes `--store-path` and the other
[layout flags](../../cli.md#autoreview-init) instead.

## 5. Point the policy at your repository

Open `$AUTOREVIEW_HOME/policy.yaml`. In the `repositories` list, set `match`
to the repository you want reviewed, in lowercase `owner/repository` form.
Leave the rest of the example unchanged for this guide. The `trigger` of the
example is `label:ai:review`, so a review starts when a pull request carries
the `ai:review` label. Create that label in the repository if it does not
exist yet.

Every notification level and the review queue need a `matrix` backend; a
route to a `none` backend is refused at load. The example routes them to one
`matrix` backend with placeholder values: set its `homeserver_url`, `room` and
`mention_user` with the [notifiers guide](notifiers.md), and export its token
as `AUTOREVIEW_MATRIX_TOKEN` in the same way as the GitHub token in step 7.

## 6. Install the agent profile

The policy names one agent profile for its reviewer in
`reviewers.<id>.agent_profile`. It must exist on the review host. Write it on
that host, as the user of its pohunek daemon, with
[`autoreview profiles install`](../../cli.md#autoreview-profiles-install). The
example reviewer uses Codex:

```sh
autoreview profiles install --host <host> --agent codex --name autoreview-codex \
  --model <model> --effort high --agent-binary "$(command -v codex)" \
  --maximum-input-bytes 1048576 --maximum-result-bytes 1048576 \
  --maximum-retention-seconds 7200
```

Its `result` is `created`, and `unchanged` when you run it again. Keep
`--maximum-result-bytes` at most `limits.result_max_bytes` and
`--maximum-retention-seconds` at least `session.timeout_seconds` of the
configuration.

The base runtime must already be installed on that host, see step 1.

## 7. Provide the GitHub token

The example configuration has one credential that covers every repository and reads its token from
`AUTOREVIEW_GH_TOKEN`. Export the token from your secret store into the shell that
runs autoreview. Do not print it, paste it into a file, or put it in the
configuration. To check that it is set without showing it:

```sh
test -n "$AUTOREVIEW_GH_TOKEN" && echo set
```

autoreview gives this token only to the `gh` calls for its repositories. Before
the first run, the token must belong to the `login` of that credential in
`config.yaml`, the value you set in step 4. `serve` refuses to start otherwise,
with `configuration_invalid` naming `github.credentials.login`.

The Matrix backend of step 5 reads its token from the variable named by its
`access_token_env`, for example `AUTOREVIEW_MATRIX_TOKEN`. Export it the same
way and check it with `test -n "$AUTOREVIEW_MATRIX_TOKEN" && echo set`.

## 8. Validate the configuration

Point autoreview at the configuration and check it. `validate` loads the
config and the policy, reads every prompt and checks that the token variables
are set. It makes no GitHub or pohunek call.

```sh
export AUTOREVIEW_CONFIG="$AUTOREVIEW_HOME/config.yaml"
autoreview validate
```

A valid setup prints one JSON object with `policy_digest` and
`agent_profiles`. An error prints `error` with a code
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

The reviewer of this example reads the pull request through git from a
worktree at the base branch, and pohunek fetches only the base branch. Put the
head into the project repository on the review host before the review starts:

```sh
git -C <project repository> fetch origin refs/pull/<pr-number>/head
```

Without it the adapter refuses the run and the review stops with
`review_head_unavailable` (see
[Head commit unavailable](../runbooks/troubleshooting.md#head-commit-unavailable)).

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

Push a new commit to the pull request, and fetch the new head on the review
host as in step 10. After the debounce period, the review runs again. The
reviewer of this example is Codex, and it reviews only the new commits and
settles each earlier blocking finding as fixed, still open or
answered. See [incremental reviews](../concepts/incremental-reviews.md).

## 15. Run as a service

Once one review works, run autoreview as a long-running service so it reviews
without manual polls. [`scripts/setup`](getting-started.md) (after 0.2.0) writes and
starts user units on one machine. On a server, write the systemd units of install path b, or use the
container image that is planned in #40, as described in
[install](../../install.md), and let `autoreview serve` run until you stop it.
Reviews that are running at shutdown resume after the next start.

Before you leave this setup running, read the
[threat model](../safety/threat-model.md). Repositories with untrusted
authors need the rules from [public repositories](public-repositories.md).
`trusted: any` reviews every author, forks included, with the rule's outcomes;
use it only for a repository without outside contributors (see
[policy: authors](../../policy.md#authors)).

## Next steps

- Give each level its own Matrix room: [notifiers](notifiers.md).
- Add an approval step: [human approval](human-approval.md).
- Run reviewers on a second host: [multihost](multihost.md).
