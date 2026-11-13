---
type: Runbook
id: runbook/upgrade-and-backup
title: Upgrade and backup
description: Back up the SQLite store, upgrade the binary or the container image (planned in #40), validate before the restart, and roll back, with the pohunek version compatibility rules.
source_kind: manual
intents: [update, help]
---

# Upgrade and backup

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0. The container image this page
> mentions is not in 0.2.0 and is planned in #40.

Back up first, validate before every restart, and keep the previous binary or
image until the new version has published a review.

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

## Upgrade the container

The container image is not in 0.2.0 and is planned in #40; this section
describes its design.

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

Not available yet: there is no workflow plugin. Planned in #41.

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

A rollback goes to an earlier autoreview release (Rust).

## pohunek version compatibility

A check of `pohunek --version` in `validate` is planned in #9. The engine uses
`project list` and `session new|output|rm`, so a pohunek upgrade is an upgrade
of autoreview's dependency too:

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
