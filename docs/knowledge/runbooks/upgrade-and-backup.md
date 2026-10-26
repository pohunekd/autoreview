---
type: Runbook
id: runbook/upgrade-and-backup
title: Upgrade and backup
description: Back up the SQLite store, upgrade the binary or container image, validate before the restart, and roll back, with the pohunek version compatibility rules.
source_kind: manual
intents: [update, help]
---

# Upgrade and backup

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

Back up first, validate before every restart, and keep the previous binary or
image until the new version has published a review.

## Back up the store

All state is in one SQLite file, `store.path` in `config.yaml`. Copy it with the
built-in command, which is consistent while the service runs:

```sh
sudo -u autoreview autoreview --config /etc/autoreview/config.yaml backup-db /var/backups/autoreview/autoreview-$(date +%F).db
```

The backup holds review data, not secrets, but it names repositories and
pull requests. Keep the directory mode `0700`.

Take a backup before every upgrade, and before any change to the store's
retention settings (`store.review_retention_seconds`,
`store.published_retention_seconds`).

## Upgrade the binary

1. Download and verify the new release as on the [install page](../../install.md#a-release-binary).
2. Keep the old binary: `sudo cp /usr/local/bin/autoreview /usr/local/bin/autoreview.previous`.
3. Install the new binary over the old one, then run `validate` with the new
   binary, while the old service still runs:

   ```sh
   sudo install -m 0755 autoreview /usr/local/bin/autoreview
   sudo -u autoreview autoreview --config /etc/autoreview/config.yaml validate
   ```

   `validate` loads `config.yaml`, the policy and every prompt, and checks that
   the pohunek CLI answers at a supported version. A new key or a changed
   policy rule shows up here as a named error. Fix it before the restart.
4. Compare `agent_profiles` in the output with the profiles on each review host.
   A profile the policy now needs is created with `autoreview profiles install`.
5. Restart the service and check its state:

   ```sh
   sudo systemctl restart autoreview.service
   sudo systemctl status autoreview.service --no-pager
   ```

Reviews running at shutdown resume on the new process. See
[retry and recovery](retry-and-recovery.md#restarts).

## Upgrade the container

Change the image tag in the compose file, pull, and validate with the same
mounts and environment file as the service:

```sh
docker compose pull
docker compose run --rm autoreview --config /etc/autoreview/config.yaml validate
docker compose up -d
```

The `validate` run uses the new image, so the pohunek version it checks is the
one bundled in that image. A tag pins the version; do not use `latest` for a
service that reviews code.

## Upgrade the workflow plugin

On a machine that runs autoreview as the pohunek workflow plugin, back up the store
first. The plugin's action list has no `backup-db`, so run it with an autoreview
binary of the current version, as the owner user, with `--config` pointing at the
plugin's `config.yaml`. Find the configuration directory with
`pohunek workflow inspect pohunekd.autoreview`.

1. Keep the current archive, `autoreview-<current-version>.tar.zst`, so that you can reinstall it.
2. Install the new archive as in [path d](../../install.md#d-pohunek-workflow-plugin),
   first without `--yes` to review it, then with `--yes`:

   ```sh
   pohunek workflow install autoreview-<new-version>.tar.zst --sha256 <digest>
   pohunek workflow install autoreview-<new-version>.tar.zst --sha256 <digest> --yes
   ```

3. Validate the configuration with the new version, and compare its `agent_profiles`
   with the profiles on each review host, as in steps 3 and 4 of the binary upgrade:

   ```sh
   pohunek workflow run --json pohunekd.autoreview validate
   ```

4. Check that the service runs the new version and that the next review publishes.
   The reference does not name a status command for plugin services.

To roll back, install the kept archive again with `--yes`. Restore the backup over
`store.path` too, but only if the new version may have changed the store, as in
[roll back](#roll-back).

## Store migrations

The reference pages do not describe store migrations. It is not documented
whether a new release changes the SQLite schema, or whether such a change runs
on the first start. Until the release notes say so, treat every upgrade that
changes the version as one that may migrate the store. Keep the backup from
the first section until a published review on the new version confirms that
the service works.

## Roll back

Roll back only with a backup taken before the upgrade:

1. Stop the service: `sudo systemctl stop autoreview.service`.
2. Restore the old binary (`/usr/local/bin/autoreview.previous`) or set the
   previous image tag in the compose file.
3. If the new version may have changed the store, restore the backup over
   `store.path` while the service is stopped. Reviews published after the backup
   are not in the restored store; check them on GitHub before you retry.
4. Run `validate` with the old version, then start the service.

## pohunek version compatibility

`validate` checks `pohunek --version` against the range this autoreview release
supports, and checks the commands the engine uses (`project list`,
`session new|output|rm`). A pohunek upgrade is therefore an upgrade of
autoreview's dependency too:

- After a pohunek upgrade on a review host, run `validate` on the autoreview
  host. A version outside the range is refused.
- The minimum version is raised after an upstream fix. Two issues drive the
  workarounds: [zajca/pohunek#1437](https://github.com/zajca/pohunek/issues/1437)
  (base fetch lock) and [zajca/pohunek#1438](https://github.com/zajca/pohunek/issues/1438)
  (branches kept after `session rm`). Until both are fixed, the workarounds stay
  on, and the stale-branch cleanup stays a manual maintenance step.
- The supported range itself is not published in these pages yet.

## Related pages

- [Install](../../install.md)
- [Retry and recovery](retry-and-recovery.md)
- [Stale sessions and branches](stale-sessions-and-branches.md)
- [Troubleshooting](troubleshooting.md)
- [Configuration reference](../../configuration.md#store)
