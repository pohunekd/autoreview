---
kind: added
---
`autoreview eval add --from-finding OWNER/NAME#N:FINDING_KEY --verdict wrong|right --store PATH` writes an eval case from a published finding that was refuted or open at merge: `wrong` gives a clean case, `right` a defect case in the finding's path with your `--lines` and `--description`. `--store` opens the production database read-only: it creates no database file or directory, runs no migration and takes no engine or discovery lock, so `serve` can run beside it (without `serve`, SQLite leaves `-wal` and `-shm` files beside the database); with it, `eval add --from-pr` takes the base and head of the newest successful review from the store.
