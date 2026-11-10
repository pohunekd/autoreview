---
type: Runbook
id: runbook/upgrade-and-backup
title: Upgrade and backup
description: Back up the SQLite store, upgrade the binary or the container image (after 0.1.0, planned in #40), validate before the restart, and roll back, with the pohunek version compatibility rules.
source_kind: manual
intents: [update, help]
---

# Upgrade and backup

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0. The container image this page
> mentions comes after 0.1.0 and is planned in #40.

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

1. Download, verify and unpack the new release archive as on the
   [install page](../../install.md#a-release-binary), with `version` and `target` set.
2. Keep the old binary: `sudo cp /usr/local/bin/autoreview /usr/local/bin/autoreview.previous`.
3. Install the new binary over the old one, then run `validate` with the new
   binary, while the old service still runs:

   ```sh
   sudo install -m 0755 "autoreview-$version-$target/autoreview" /usr/local/bin/autoreview
   sudo -u autoreview autoreview --config /etc/autoreview/config.yaml validate
   ```

   `validate` loads `config.yaml`, the policy and every prompt. A new key or a changed
   policy rule shows up here as a named error. Fix it before the restart.
4. Compare `agent_profiles` in the output with the profiles on each review host.
   Create a profile the policy now needs on each review host; `autoreview
   profiles install` is planned in #25.
5. Restart the service and check its state:

   ```sh
   sudo systemctl restart autoreview.service
   sudo systemctl status autoreview.service --no-pager
   ```

Reviews running at shutdown resume on the new process. See
[retry and recovery](retry-and-recovery.md#restarts).

## Upgrade the container

The container image comes after 0.1.0 and is planned in #40; this section
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

## Switching from the Go engine

The Go engine reads `HERMES_PR_REVIEW_*` variables from its environment files;
autoreview reads `config.yaml`. Both use the same SQLite store, so the switch
keeps every review, and the Go engine can take the store back as long as no
release has changed the store schema.

The steps below use these names; replace them with yours:

- `<go-unit>`: the systemd unit of the Go engine.
- `<go-timer>`: the timer that runs the Go engine's `review-queue`, if there is one;
  leave it out of the commands otherwise. `<go-queue-service>` is the service it starts.
- `<go-binary>`: the binary the Go units run. If it is `/usr/local/bin/autoreview`,
  step 1 of the preparation copies it to `/usr/local/bin/autoreview-go`, and
  `<go-binary>` means that copy.
- `<env-file>`: an `EnvironmentFile=` of `<go-unit>` that holds
  `HERMES_PR_REVIEW_*` variables.
- `<secrets-file>`: an `EnvironmentFile=` of `<go-unit>` that also holds
  variables of other services, such as a secrets file the host shares. If the
  unit has none, leave it out.
- `<user>` and `<group>`: the user and group of `<go-unit>`.
- `<store>`: the store, the Go engine's `HERMES_PR_REVIEW_STATE_DB_PATH`, and
  `<store-dir>` its directory.
- `<pohunek-state>` and `<pohunek-runtime>`: the Go engine's
  `HERMES_PR_REVIEW_POHUNEK_STATE_HOME` and `HERMES_PR_REVIEW_POHUNEK_RUNTIME_DIR`.
- `<gh-bin>` and `<pohunek-bin>`: the Go engine's `HERMES_PR_REVIEW_GH_BIN` and
  `HERMES_PR_REVIEW_POHUNEK_BIN`, which become `github.gh_bin` and `pohunek.bin`.

When the unit has more environment files, name each of them, in the unit's
order, wherever these steps name `<env-file>` and `<secrets-file>`: once per
file in the drop-in, and as `-p EnvironmentFile=` in every `systemd-run`
command. A file left out changes what the engines read: the Go engine stops
at a missing variable, and an override in a later file, such as another
`HERMES_PR_REVIEW_STATE_DB_PATH`, would point a command at another store.

Run every command as root. The `systemd-run` commands pass the unit's user and
environment files, so that each engine reads the values its service reads.
They do not apply the sandbox of a unit; step 5 of the preparation checks
autoreview inside it.

### Prepare while the Go engine runs

1. Never install autoreview over a path a Go unit runs. If `<go-unit>` or
   `<go-queue-service>` runs `/usr/local/bin/autoreview`, copy that binary and
   point both units at the copy first:

   ```sh
   install -o root -g root -m 0755 /usr/local/bin/autoreview /usr/local/bin/autoreview-go
   systemctl edit <go-unit>
   systemctl edit <go-queue-service>
   ```

   In each drop-in, repeat the unit's `ExecStart=` with only the binary
   changed, for example:

   ```ini
   [Service]
   ExecStart=
   ExecStart=/usr/local/bin/autoreview-go serve
   ```

   Then run `systemctl daemon-reload` and check with `systemctl cat <go-unit>
   <go-queue-service>` that both run `/usr/local/bin/autoreview-go`. Every
   later start of the Go engine, a restart during the preparation and the
   rollback included, now runs the copy. Do not name the copy
   `autoreview.previous`: an upgrade of autoreview keeps its own previous binary
   under that name ([upgrade the binary](#upgrade-the-binary), step 2) and would
   replace the Go engine with autoreview. Keep the copy and the drop-ins until
   the Go engine is retired (step 5 of the switch).
2. Install the autoreview binary as `/usr/local/bin/autoreview` and the unit
   files from the [install page](../../install.md#b-systemd-units). Do not
   start them. The units run in a sandbox: `ProtectSystem=strict` makes every
   path outside `ReadWritePaths=` read-only, `ProtectHome=yes` hides `/home`,
   `/root` and `/run/user`, and `PrivateTmp=yes` gives the service its own
   `/tmp`. So none of `<store>`, `config.yaml`, the policy file, its prompts,
   `<gh-bin>`, `<pohunek-bin>`, `<pohunek-state>` and `<pohunek-runtime>` may be
   under those directories, nor any home directory that the `gh` or pohunek
   CLI reads, such as the home of `<user>`. The service must be able to write
   `<store-dir>` (SQLite writes `<store>-wal` and `<store>-shm` next to the
   store) and the pohunek directories.
3. Give both services the Go engine's user, paths and environment, and its two
   secret variables. autoreview reads tokens only from `AUTOREVIEW_*`
   variables, and the Go engine ignores those, so a rollback still works on the
   same files. Map them at start, so that no secret is copied: with
   `systemctl edit autoreview.service`, add

   ```ini
   [Service]
   User=<user>
   Group=<group>
   ReadWritePaths=<store-dir> <pohunek-state> <pohunek-runtime>
   EnvironmentFile=
   EnvironmentFile=<env-file>
   EnvironmentFile=<secrets-file>
   ExecStart=
   ExecStart=/bin/sh -c 'export AUTOREVIEW_GH_TOKEN="$$GH_TOKEN" \
     AUTOREVIEW_MATRIX_TOKEN="$$MATRIX_ACCESS_TOKEN"; \
     exec /usr/local/bin/autoreview --config /etc/autoreview/config.yaml serve'
   ```

   and the same for `autoreview-queue.service`, with `review-queue` in place of
   `serve`. systemd turns `$$` into `$`, so the shell reads the tokens and they
   never appear on a command line. Alternatively, add the lines
   `AUTOREVIEW_GH_TOKEN=` and `AUTOREVIEW_MATRIX_TOKEN=` with the token values in
   an editor to the file that already holds `GH_TOKEN`, and only when that file
   is owned by root with mode `0600`, or `0640` with group `<group>`. Never add
   them to a settings file that others may read. A token change then edits both
   lines.
4. Write `config.yaml` with the conversion script from the autoreview source
   tree of the release. Name every file of `<go-unit>` in the unit's order, a
   settings file with `--settings-env` and a shared file with `--secrets-env`:

   ```sh
   python3 -I scripts/env-to-config /etc/autoreview/config.yaml \
     --settings-env <env-file> --secrets-env <secrets-file>
   chgrp <group> /etc/autoreview/config.yaml
   chmod 0640 /etc/autoreview/config.yaml
   ```

   The script maps every `HERMES_PR_REVIEW_*` variable to its key, writes one
   Matrix backend per distinct room, and creates the file with mode `0600`. It
   never overwrites a file and never decodes, prints or writes a secret value:
   `config.yaml` names `AUTOREVIEW_GH_TOKEN` and `AUTOREVIEW_MATRIX_TOKEN`
   only. It stops at the first variable it cannot convert and names it, or the
   file and line. It accepts plain `NAME=value` lines and comments with LF line
   ends; quote a value whole in `"..."` or `'...'` and do not use backslashes.
   It stops at a variable it does not know in a settings file. In a
   `--secrets-env` file it reads only the tokens, `MATRIX_HOMESERVER` and
   `HERMES_PR_REVIEW_*`. Of the tokens and of every other assignment it checks
   only that the value ends on its line, because an open quote or a final
   backslash would carry it into the next line; it never decodes the value
   itself. A later file wins, as in systemd.
5. Check autoreview in the sandbox of its unit while the Go engine still runs.
   The properties repeat the lines of `autoreview.service` and of the drop-in of
   step 3 that decide which paths the service reaches; keep them equal when you
   change the unit. The command leaves out `CapabilityBoundingSet=`,
   `StateDirectory=` and `RuntimeDirectory=`, which do not change that. It
   checks that the service can run both CLIs and write the store, its directory
   and the pohunek directories, then runs `validate`:

   ```sh
   systemd-run --pipe --wait --quiet -p User=<user> -p Group=<group> \
     -p EnvironmentFile=<env-file> -p EnvironmentFile=<secrets-file> \
     -p NoNewPrivileges=yes -p ProtectSystem=strict -p ProtectHome=yes -p PrivateTmp=yes \
     -p 'ReadWritePaths=<store-dir> <pohunek-state> <pohunek-runtime>' \
     /bin/sh -c 'test -x <gh-bin> && test -x <pohunek-bin> || exit 1
       test -w <store> && test -w <store-dir> || exit 1
       test -w <pohunek-state> && test -w <pohunek-runtime> || exit 1
       export AUTOREVIEW_GH_TOKEN="$$GH_TOKEN" AUTOREVIEW_MATRIX_TOKEN="$$MATRIX_ACCESS_TOKEN"
       exec /usr/local/bin/autoreview --config /etc/autoreview/config.yaml validate'
   ```

   The output names the policy digest and the agent profiles. An exit status
   of 1 with no output means that one of the two CLIs cannot be run, or that
   the store, its directory or a pohunek directory cannot be written in the
   sandbox. Fix any error before you go on.

### Switch

1. Drain: wait until no review is `queued`, `running` or `awaiting`. Pick a
   quiet time; an `awaiting` review waits for a human decision. The first
   command prints nothing and the second prints `[]` when the store is drained:

   ```sh
   systemd-run --pipe --wait --quiet -p User=<user> \
     -p EnvironmentFile=<env-file> -p EnvironmentFile=<secrets-file> \
     <go-binary> reviews | grep -E '"status":"(queued|running|awaiting)"'
   systemd-run --pipe --wait --quiet -p User=<user> \
     -p EnvironmentFile=<env-file> -p EnvironmentFile=<secrets-file> \
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
   systemd-run --pipe --wait --quiet -p User=<user> \
     -p EnvironmentFile=<env-file> -p EnvironmentFile=<secrets-file> \
     <go-binary> backup-db /var/backups/autoreview/before-switch.db
   ```

4. Start autoreview on the same store (`store.path` is `<store>`):

   ```sh
   systemctl enable --now autoreview.service autoreview-queue.timer
   systemctl status autoreview.service --no-pager
   ```

5. Keep the backup, `<go-binary>` and the Go drop-ins at least until
   autoreview has published a review. Removing `<go-binary>` ends the option to
   roll back. When you retire the Go engine, remove the drop-ins of step 1 of
   the preparation in the same change, and keep the Go units disabled or remove
   them: without the drop-ins they would start `/usr/local/bin/autoreview`.

   ```sh
   rm /etc/systemd/system/<go-unit>.d/override.conf
   rm /etc/systemd/system/<go-queue-service>.d/override.conf
   systemctl daemon-reload
   rm /usr/local/bin/autoreview-go
   ```

### Roll back to the Go engine

1. Stop autoreview: `systemctl disable --now autoreview.service autoreview-queue.timer`.
2. When the release notes name a newer store schema than the Go engine reads,
   restore the backup. Move the store and its SQLite side files aside, then
   install the backup with the Go engine's owner:

   ```sh
   for file in <store> <store>-wal <store>-shm; do
     if [ -e "$file" ]; then mv "$file" "$file.after-switch"; fi
   done
   install -o <user> -g <group> -m 0600 /var/backups/autoreview/before-switch.db <store>
   ```

   Reviews published after the backup are not in the restored store; check
   them on GitHub before you retry them. Otherwise keep the store as it is.
3. When a rule uses a `codex-envelope` reviewer, point its Codex profiles
   back at the older Codex wrapper. The Go engine refuses the report of the
   adapter in `adapters/codex/`, because it carries `blocking` and
   `prior_findings`. A Codex review that is in flight with prior findings
   stops; retry it after the Go engine runs.
4. Start the Go engine on the same environment files:
   `systemctl enable --now <go-unit> <go-timer>`. With the drop-ins of step 1
   of the preparation, both units run `<go-binary>`, not autoreview.

### Known differences

None of these changes what the Go engine has written. Where they differ,
autoreview refuses what the Go engine read leniently.

Reviewers:

- A `codex-envelope` reviewer must run the adapter in `adapters/codex/`.
  autoreview refuses the report of the older Codex wrapper with
  `review_adapter_outdated`, and the Go engine refuses the report of the
  adapter. Switch every Codex profile to the adapter in the same maintenance
  window as the upgrade, and switch it back when you roll back.
- The Go engine treats every Codex finding as blocking. On the first Codex
  re-review after the upgrade, every finding of the last review published
  before it is a prior finding that the reviewer must settle.

Configuration:

- A Matrix homeserver URL with an empty port or a port above 65535, and a
  store path of `/`, are refused; the Go engine accepted both.
- The tokens are read from `AUTOREVIEW_GH_TOKEN` and `AUTOREVIEW_MATRIX_TOKEN`
  (step 3 of the preparation).
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
