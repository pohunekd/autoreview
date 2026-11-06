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

## Switching from the Go engine

The Go engine reads `HERMES_PR_REVIEW_*` variables from its environment files;
autoreview reads `config.yaml`. Both use the same SQLite store, so the switch
keeps every review, and the Go engine can take the store back as long as no
release has changed the store schema.

The steps below use these names; replace them with yours:

- `<go-unit>`: the systemd unit of the Go engine.
- `<go-timer>`: the timer that runs the Go engine's `review-queue`, if there is one;
  leave it out of the commands otherwise.
- `<go-binary>`: the Go engine's binary. If it is `/usr/local/bin/autoreview`,
  copy it to `/usr/local/bin/autoreview.previous` first and use that path.
- `<env-file>`: an `EnvironmentFile=` of `<go-unit>` that holds
  `HERMES_PR_REVIEW_*` variables.
- `<secrets-file>`: an `EnvironmentFile=` of `<go-unit>` that also holds
  variables of other services, such as a secrets file the host shares. If the
  unit has none, leave it out.

When the unit has more files, name each of them, in the unit's order, wherever
these steps name `<env-file>` and `<secrets-file>`: once per file in the
drop-in, and as `-p EnvironmentFile=` in every `systemd-run` command.
- `<user>`: the user of `<go-unit>`.

Every command loads the environment the way the service does, through
`systemd-run`, so that both engines see the same values. Run them as root.

### Prepare while the Go engine runs

1. Install the autoreview binary as `/usr/local/bin/autoreview` and the unit
   files from the [install page](../../install.md#b-systemd-units). Do not
   start them. Set `User=`, `Group=` and `ReadWritePaths=` of both services to
   the user and the paths of `<go-unit>`, so that autoreview can write the store.
2. Give autoreview its two secret variables. It reads tokens only from
   `AUTOREVIEW_*` variables, and the Go engine ignores those, so a rollback
   still works on the same files. Map them at start, so that no secret is
   copied: with `systemctl edit autoreview.service`, add

   ```ini
   [Service]
   EnvironmentFile=
   EnvironmentFile=<env-file>
   EnvironmentFile=<secrets-file>
   ExecStart=
   ExecStart=/bin/sh -c 'AUTOREVIEW_GH_TOKEN="$$GH_TOKEN" AUTOREVIEW_MATRIX_TOKEN="$$MATRIX_ACCESS_TOKEN" exec /usr/local/bin/autoreview --config /etc/autoreview/config.yaml serve'
   ```

   and the same for `autoreview-queue.service`, with `review-queue` in place of
   `serve`. systemd turns `$$` into `$`, so the shell reads the tokens and they
   never appear on a command line. Alternatively, add the lines
   `AUTOREVIEW_GH_TOKEN=` and `AUTOREVIEW_MATRIX_TOKEN=` with the token values to
   `<env-file>` in an editor; then a token change edits both lines.
3. Write `config.yaml` with the conversion script from the autoreview source
   tree of the release. Name every file of `<go-unit>` in the unit's order, a
   settings file with `--settings-env` and a shared file with `--secrets-env`:

   ```sh
   python3 -I scripts/env-to-config /etc/autoreview/config.yaml \
     --settings-env <env-file> --secrets-env <secrets-file>
   chgrp <user> /etc/autoreview/config.yaml
   chmod 0640 /etc/autoreview/config.yaml
   ```

   The script maps every `HERMES_PR_REVIEW_*` variable to its key, writes one
   Matrix backend per distinct room, and creates the file with mode `0600`. It
   never overwrites a file and never reads, prints or writes a secret value:
   `config.yaml` names `AUTOREVIEW_GH_TOKEN` and `AUTOREVIEW_MATRIX_TOKEN`
   only. It stops at the first variable it cannot convert and names it, or the
   file and line. It accepts plain `NAME=value` lines and comments; quote a
   value whole in `"..."` or `'...'` and do not use backslashes. It stops at a
   variable it does not know in a settings file. In a `--secrets-env` file it
   reads only the tokens, `MATRIX_HOMESERVER` and `HERMES_PR_REVIEW_*`. Of every
   other assignment it checks only that the value ends on its line, because an
   open quote or a final backslash would carry it into the next line; it never
   reads the value itself. A later file wins, as in systemd.
4. Validate with autoreview while the Go engine still runs:

   ```sh
   systemd-run --pipe --wait --quiet -p EnvironmentFile=<env-file> -p User=<user> \
     /bin/sh -c 'AUTOREVIEW_GH_TOKEN="$$GH_TOKEN" AUTOREVIEW_MATRIX_TOKEN="$$MATRIX_ACCESS_TOKEN" exec /usr/local/bin/autoreview --config /etc/autoreview/config.yaml validate'
   ```

   The output names the policy digest and the agent profiles. Fix any named
   error before you go on.

### Switch

1. Drain: wait until no review is `queued`, `running` or `awaiting`. Pick a
   quiet time; an `awaiting` review waits for a human decision. The first
   command prints nothing and the second prints `[]` when the store is drained:

   ```sh
   systemd-run --pipe --wait --quiet -p EnvironmentFile=<env-file> -p User=<user> \
     <go-binary> reviews | grep -E '"status":"(queued|running|awaiting)"'
   systemd-run --pipe --wait --quiet -p EnvironmentFile=<env-file> -p User=<user> \
     <go-binary> awaiting
   ```

   `reviews` lists only the newest `HERMES_PR_REVIEW_REVIEW_LIST_LIMIT` reviews,
   so an older `queued` or `running` review is not shown; when the store holds
   more reviews than that, raise the limit in a copy of `<env-file>` for this
   check. `awaiting` lists every waiting review regardless of age.
2. Stop the Go engine: `systemctl disable --now <go-unit> <go-timer>`. Run
   the checks of step 1 again; if a review appeared, start `<go-unit>` and
   drain again.
3. Back up the store with the Go engine's own command, into a directory with
   mode `0700` that `<user>` can write. Let it finish: do not interrupt it. It
   prints `{"ok":true,...}` when the copy is complete.

   ```sh
   systemd-run --pipe --wait --quiet -p EnvironmentFile=<env-file> -p User=<user> \
     <go-binary> backup-db /var/backups/autoreview/before-switch.db
   ```

4. Start autoreview on the same store (`store.path` is the Go engine's
   `HERMES_PR_REVIEW_STATE_DB_PATH`):

   ```sh
   systemctl enable --now autoreview.service autoreview-queue.timer
   systemctl status autoreview.service --no-pager
   ```

5. Keep the backup and `<go-binary>` until autoreview has published a review.

### Roll back to the Go engine

1. Stop autoreview: `systemctl disable --now autoreview.service autoreview-queue.timer`.
2. Start the Go engine on the same store and environment files:
   `systemctl enable --now <go-unit> <go-timer>`.

This works while the autoreview release writes the store schema the Go engine
reads. When the release notes name a newer store schema, stop both engines,
restore the backup over `store.path` instead, and check on GitHub the reviews
published after the backup.

### Known differences

None of these changes what the Go engine has written. Where they differ,
autoreview refuses what the Go engine read leniently.

Configuration:

- A Matrix homeserver URL with an empty port or a port above 65535, and a
  store path of `/`, are refused; the Go engine accepted both.
- The tokens are read from `AUTOREVIEW_GH_TOKEN` and `AUTOREVIEW_MATRIX_TOKEN`
  (step 2 of the preparation).
- The conversion script refuses a timeout or retention that does not fit in
  the Go engine's duration type (above 9223372036 seconds); the Go engine
  wrapped such values.
- A policy file with a float in an integer field, a `null` list entry, no
  content or only comments, or bytes that are not UTF-8 is refused.

Stored data, which the Go engine never writes in these forms and which only a
hand edit or an older engine leaves:

- Rows from an earlier engine that use retired event names need
  [#186](https://github.com/pohunekd/autoreview/issues/186) merged before the
  switch.
- JSON `null` in a string, scalar, enum, struct or map field, a lone surrogate
  escape, `-0` in an integer field, keys that differ only in case, duplicate
  keys, JSON nested deeper than 128 levels, invalid UTF-8, a time outside the
  years -9999 to 9999, and an unknown decision, outcome or enum value are
  refused with `store_state_invalid` or `store_unavailable`. A review whose
  stored input cannot be decoded stops instead of being retried.
- A slot without a `run` key reads as run 0, not run 1.

Running reviews:

- A verifier output with a `null` verdict or note, or a case-variant key,
  publishes the review unverified at once; the Go engine ran the verifier again.
- Waiting for a busy discovery lock ends with `discovery_busy`, not
  `workflow_failed`.
- A non-ASCII login, repository name or pull request URL is compared exactly;
  the Go engine folded case.
- During shutdown, a cancelled `gh` or pohunek call reports
  `<binary>: context canceled` without its standard error, and one notification
  may be sent between the cancellation and the next store guard.
- The JSON sent to `gh` writes `<`, `>`, `&`, U+2028 and U+2029 unescaped;
  GitHub receives the same values.

Commands:

- A signal during `backup-db`, `reviews`, `awaiting`, `resolve`, `approve` or
  `review` waits for the running store call; the Go engine cancelled it. Do not
  interrupt a running `backup-db`.
- A closed standard output, as in `autoreview reviews | head -c 0`, ends with
  `workflow_failed` and exit status 1; the Go engine died by SIGPIPE (141).
- `--help`, `-h` and `-V` print the command help or version and exit 0; the Go
  engine printed an error object and exited 1. Arguments are checked before
  the configuration loads.
- A non-UTF-8 argument of `retry`, `resolve`, `approve` or `review` is refused
  as `command_invalid`.

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
