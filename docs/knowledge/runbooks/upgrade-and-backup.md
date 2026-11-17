---
type: Runbook
id: runbook/upgrade-and-backup
title: Upgrade and backup
description: Back up the SQLite store, upgrade the binary, validate before the restart, and roll back, with the pohunek version compatibility rules.
source_kind: manual
intents: [update, help]
---

# Upgrade and backup

> **Status: after 0.6.0.** This page describes autoreview on `main` after the
> 0.6.0 release. A part that is not available names its open issue. A container
> image is not available (#40).

Back up first, validate before every restart, and keep the previous binary
until the new version has published a review.

## Back up the store

All state is in one SQLite file, `store.path` in `config.yaml`. Copy it with the
built-in command, which is consistent while the service runs:

```sh
sudo -u autoreview autoreview --config /etc/autoreview/config.yaml \
  --env-file /etc/autoreview/autoreview.env \
  backup-db /var/backups/autoreview/autoreview-$(date +%F).db
```

The backup holds review data, not secrets, but it names repositories and
pull requests. Keep the directory mode `0700`.

Take a backup before every upgrade, and before any change to the store's
retention settings (`store.review_retention_seconds`,
`store.published_retention_seconds`, `store.outcome_retention_seconds`).

## Upgrade the binary

1. Download, verify and unpack the new release archive as on the
   [install page](../../install.md#a-release-binary), with `version` and `target` set.
2. Keep the old binary: `sudo cp /usr/local/bin/autoreview /usr/local/bin/autoreview.previous`.
3. Install the new binary over the old one, then run `validate` with the new
   binary, while the old service still runs:

   ```sh
   sudo install -m 0755 "autoreview-$version-$target/autoreview" /usr/local/bin/autoreview
   sudo -u autoreview autoreview --config /etc/autoreview/config.yaml \
     --env-file /etc/autoreview/autoreview.env validate
   ```

   `validate` loads `config.yaml`, the policy and every prompt. A new key or a changed
   policy rule shows up here as a named error. Fix it before the restart. From
   0.5.0 on it also refuses a store directory or store file that grants the group
   or other users any access, and names the `chmod` that fixes each path. A
   store that a command run by hand created, such as a first
   `sudo -u autoreview autoreview poll` under umask 022, has 0644 files; run
   `sudo -u autoreview sh -c 'chmod 600 /var/lib/autoreview/autoreview.db*'`
   before the upgrade, or `deploy/install.sh` stops at its `validate` step with
   exit code 4 and replaces nothing. The glob is inside `sh -c` because only
   `autoreview` can list the 0700 directory.
4. Compare `agent_profiles` in the output with the profiles on each review host.
   On each review host, install the new binary as well and run
   [`autoreview profiles install`](../../cli.md#autoreview-profiles-install)
   again for every profile, with the flags it was written with (an upgrade from
   0.3.0 or earlier adds `--pohunek-timeout-seconds`): `updated` means
   the new binary carries a newer adapter. Write a profile the policy now needs
   the same way. Install every Claude profile right before the restart: the
   engine sends its Claude requests as `autoreview-claude/v1`, which a Claude
   adapter of 0.3.0 or earlier runs as a plain prompt, and stops each such
   review with `review_adapter_outdated`; the engine also refuses the result of
   an older Claude adapter that names no checked checkout. The newer Claude
   adapter refuses the `checkout: base` reviews of an engine whose requests
   carry no trusted checkout lines, so keep the window short. Run
   `autoreview retry` for a pull request whose review stopped in that window.
   Install every Codex profile before the restart:
   the engine binds each Codex run to a nonce that only the adapter of this
   release writes back, and fails the runs of an older one with
   `review_nonce_mismatch`. The Codex adapter of this release may return a
   finding `category`, which an engine from before this release refuses with
   `review_result_invalid`, so keep the time between the install and the
   restart short.
5. Restart the service and check its state:

   ```sh
   sudo systemctl restart autoreview.service
   sudo systemctl status autoreview.service --no-pager
   ```

Reviews running at shutdown resume on the new process. See
[retry and recovery](retry-and-recovery.md#restarts).

### With the systemd units of path b

The installer of [path b](../../install.md#b-systemd-units) replaces steps 3 and
5. It runs `validate` with the new binary before it replaces the old one, and
restarts the service only when that passed. Do steps 1 and 2, then step 4: the
`agent_profiles` come from `policy.yaml`, so `validate` of the installed binary
lists them. Then run the installer from the new archive:

```sh
sudo -u autoreview autoreview --config /etc/autoreview/config.yaml \
  --env-file /etc/autoreview/autoreview.env validate
sudo "autoreview-$version-$target/deploy/install.sh" \
  --binary "autoreview-$version-$target/autoreview"
```

Its line `autoreview.service restarted` is the restart of step 4's window. When
`validate` fails, it stops with exit code 4 and the old binary keeps running;
fix the configuration and run it again. It also installs the units of the new
archive. Check the state as in step 5.

### With scripts/setup

Back up the store first, then run `scripts/setup` from a checkout of the new
release tag with the same `--dir`. It installs the release that its
`AUTOREVIEW_VERSION` names and runs `validate` with the secrets file before it
touches the service. It keeps every agent profile that exists (`in place`), so
it does not do step 4, and step 4 comes before the restart:

1. When the script asks to restart the service on the new binary, or to
   replace the service's units or agents, decline. It stops with exit code 4,
   changes nothing of that step, and the old service keeps running.
2. Do step 4 for every profile, with the new `~/.local/bin/autoreview`.
3. Run the script again with the same `--dir` and accept its restart: the
   service still runs the older binary, so it asks again. The restart is
   `systemctl --user restart autoreview.service`; on a Mac (from 0.6.0 on,
   #513)
   `launchctl kickstart -k gui/$(id -u)/io.github.pohunekd.autoreview`, or the
   install of the agents when their files changed.

A later run that shows the service step as `in place, skipped`, with no note
that the running binary is unreadable, confirms that the service started on
the installed binary. On a Mac, `scripts/macos-check service` then checks the
rest of the state: every line `ok`. Its process check compares paths only, so
it cannot tell whether the service started before the binary was replaced.

## Store migrations

Every command that opens the store creates or extends its schema on the first
start of a new release, so treat every upgrade as one that may migrate the
store. A release refuses a store whose schema version (`PRAGMA user_version`)
is newer than its own with `store_version_unsupported`. Keep the backup from
the first section until a published review on the new version confirms that
the service works.

Release 0.4.0 adds the statistics rollup: the tables `stats_rollup`
and `stats_rollup_samples`, created on the first start without a new schema
version (`PRAGMA user_version` stays `1`). An earlier release opens such a
store and ignores the two tables, so a rollback to it needs no restore. While
the earlier release runs, the reviews it prunes and the pull requests it stops
are not added to the rollup, so the statistics of their days can be too low.
The counts per pipeline that `autoreview report --pipeline` reads are rows of
the same rollup, which an earlier release ignores. The reviews that 0.4.0 or
0.5.0 prunes have daily counts but none per pipeline, so `report --pipeline`
refuses a range with their days with `report_range_pruned`. The reviews that a
release without the rollup prunes have no rows at all, so they count as zero
for `report --pipeline` too.

The release with [finding outcomes](../concepts/finding-outcomes.md) adds the
tables `finding_outcomes` and `finding_outcome_pull_requests` (the state reads
of pull requests with open outcomes) the same way, without a new schema
version. An earlier release opens such a store and ignores the tables, so a
rollback to it needs no restore. While the earlier release runs, its
publications record no outcomes, no pull request is read for its outcomes,
and nothing prunes the tables.

The release with the [`guidance`](../../configuration.md#guidance) section adds
the tables `finding_categories` and `guidance_suggestions` the same way, without
a new schema version. An earlier release ignores them, so a rollback needs no
restore; while it runs, no category is recorded and no suggestion is made. A
`category` in a stored finding is ignored by it as an unknown key.

## Roll back

Roll back only with a backup taken before the upgrade:

1. Stop the service: `sudo systemctl stop autoreview.service` (with
   `scripts/setup`, `systemctl --user stop autoreview.service`; on a Mac,
   `launchctl bootout gui/$(id -u)/io.github.pohunekd.autoreview`, and
   `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.pohunekd.autoreview.plist`
   to start it again after the steps below; the service can take 90 s to stop,
   and the bootstrap fails while
   `launchctl print gui/$(id -u)/io.github.pohunekd.autoreview` still finds it).
2. Restore the old binary (`/usr/local/bin/autoreview.previous`; with
   `scripts/setup`, install the binary of the old release archive as
   `~/.local/bin/autoreview`).
3. If the new version may have changed the store, restore the backup over
   `store.path` while the service is stopped. Reviews published after the backup
   are not in the restored store; check them on GitHub before you retry.
4. Restore the `config.yaml` of the old version, or remove the keys that the new
   version added: an earlier release refuses an unknown key, such as
   `limits.max_issue_characters` for a release before it, or
   `schedule.usage_limit_retry_delays_seconds` for a release before the
   automatic retry of a usage-limited review, or
   `store.outcome_retention_seconds` for a release before finding outcomes, or
   `schedule.outcome_check_interval_seconds` for a release before the outcomes at
   merge and close, or `pohunek.head_checkout_host_hooks` for a release before
   head checkouts without the repository's hooks, or `guidance` and
   `notify.routes.guidance_suggested` for a release before guidance
   suggestions. On a converted
   deployment, remove the matching `HERMES_PR_REVIEW_*` variable too, or run
   the old release's `scripts/env-to-config`, which refuses it as unknown.
   Do the same for the policy: restore the policy of the old version, or remove
   what the new version added, such as `intent_check` from the policy's
   pipelines for a release before the intent check, or `guard` from every rule
   of the policy and `guards` from `config.yaml` for a release before the guard
   sections, or `notifications.guidance_suggested` for a release before guidance
   suggestions.
5. Run `validate` with the old version, then start the service.

A rollback goes to an earlier autoreview release (Rust).

## pohunek version compatibility

`validate` and `serve` run `pohunek --version` on the CLI at `pohunek.bin` and
refuse a release outside the supported range, 0.41.0 or later, before 1.0.0,
with `pohunek_version_unsupported` (see [install](../../install.md#prerequisites)).
The engine uses `project list`, `session list` and `session new|output|rm`, so a
pohunek upgrade is an upgrade of autoreview's dependency too:

- The minimum version is raised after an upstream fix. Two issues drive the
  workarounds: [zajca/pohunek#1437](https://github.com/zajca/pohunek/issues/1437)
  (base fetch lock) and [zajca/pohunek#1438](https://github.com/zajca/pohunek/issues/1438)
  (branches kept after `session rm`). Until both are fixed, the workarounds stay
  on, and the stale-branch cleanup stays a manual maintenance step.
- Upgrade pohunek past the range only with an autoreview release that supports
  the new release; until then `serve` does not start after a restart.

## Related pages

- [Install](../../install.md)
- [Retry and recovery](retry-and-recovery.md)
- [Stale sessions and branches](stale-sessions-and-branches.md)
- [Troubleshooting](troubleshooting.md)
- [Configuration reference](../../configuration.md#store)
