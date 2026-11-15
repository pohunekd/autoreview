---
type: Runbook
id: runbook/upgrade-and-backup
title: Upgrade and backup
description: Back up the SQLite store, upgrade the binary, validate before the restart, and roll back, with the pohunek version compatibility rules.
source_kind: manual
intents: [update, help]
---

# Upgrade and backup

> **Status: after 0.3.0.** This page describes autoreview on `main` after the
> 0.3.0 release. A part that is not available names its open issue. A container
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
`store.published_retention_seconds`).

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
   policy rule shows up here as a named error. Fix it before the restart.
4. Compare `agent_profiles` in the output with the profiles on each review host.
   On each review host, install the new binary as well and run
   [`autoreview profiles install`](../../cli.md#autoreview-profiles-install)
   again for every profile, with the flags it was written with (an upgrade from
   0.3.0 or earlier adds `--pohunek-timeout-seconds`): `updated` means
   the new binary carries a newer adapter. Write a profile the policy now needs
   the same way. Do it for every Claude profile in the same window as the
   restart: the engine refuses the result of an older Claude adapter with
   `review_adapter_outdated`, and the newer Claude adapter refuses the
   `checkout: base` reviews of an engine whose requests carry no trusted
   checkout lines. Run `autoreview retry` for a pull request whose review
   stopped in that window.
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

## Store migrations

The reference pages do not describe store migrations. It is not documented
whether a new release changes the SQLite schema, or whether such a change runs
on the first start. Until the release notes say so, treat every upgrade that
changes the version as one that may migrate the store. Keep the backup from
the first section until a published review on the new version confirms that
the service works.

The statistics rollup (#264) adds the tables `stats_rollup` and
`stats_rollup_samples` on the first start, without a new schema version
(`PRAGMA user_version` stays `1`). An earlier release opens such a store and
ignores the two tables, so a rollback of this change needs no restore. While
the earlier release runs, the reviews it prunes and the pull requests it stops
are not added to the rollup, so the statistics of their days can be too low.

## Roll back

Roll back only with a backup taken before the upgrade:

1. Stop the service: `sudo systemctl stop autoreview.service`.
2. Restore the old binary (`/usr/local/bin/autoreview.previous`).
3. If the new version may have changed the store, restore the backup over
   `store.path` while the service is stopped. Reviews published after the backup
   are not in the restored store; check them on GitHub before you retry.
4. Run `validate` with the old version, then start the service.

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
