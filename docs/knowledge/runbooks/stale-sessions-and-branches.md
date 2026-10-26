---
type: Runbook
id: runbook/stale-sessions-and-branches
title: Stale sessions and branches
description: Why review branches and pohunek sessions accumulate on the review hosts, how to remove them safely with autoreview prune-branches, and how to keep a recovery list.
source_kind: manual
intents: [debug, help]
---

# Stale sessions and branches

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

Every reviewer run happens in a pohunek session on a review host, and each
session works on its own review branch. Two upstream behaviours make these
pile up. autoreview works around both in v0.1.

## Why branches accumulate

- **pohunek keeps a removed session's branch.** Removing a session with
  `pohunek session rm` does not delete its branch. Upstream issue
  [zajca/pohunek#1438](https://github.com/zajca/pohunek/issues/1438) tracks it.
- **Every start attempt has its own branch.** A base fetch that hits a locked
  ref ([zajca/pohunek#1437](https://github.com/zajca/pohunek/issues/1437)) is retried on
  a fresh branch. The branch name is
  `<session.branch_prefix><workflow>-<reviewer>-run<N>[-start<M>]`, so a single
  review can leave several branches behind.

autoreview does not delete these branches while a review uses them. Removing
them is a maintenance step, described below.

## When a session cannot be removed

After `session.cleanup_max_attempts` removal attempts, the engine raises the
event `cleanup_manual` (action level) and reports `pohunek_session_cleanup_failed`.
The session still exists on the review host. Remove it by hand:

```sh
# On the review host, as the pohunek user
pohunek session list
pohunek session rm <session-name>
```

Use the session name from the listing. Check the review first: `cleanup_manual`
is raised for a finished review, and a session that still runs a review is not
a leftover. If a session is still running, check its output before you remove it.

## Remove leftover branches

`autoreview prune-branches` lists the review branches under
`session.branch_prefix` on the review hosts that no running review uses. It
deletes nothing unless you pass `--yes`. Always run the dry run first.

```sh
# Dry run: lists the branches that would be removed
autoreview --config /etc/autoreview/config.yaml prune-branches

# Limit to one host or to older branches
autoreview --config /etc/autoreview/config.yaml prune-branches --host <review-host> --older-than-days 14

# Delete, and keep the printed list of branch and commit pairs
autoreview --config /etc/autoreview/config.yaml prune-branches --yes > prune-$(date +%F).json
```

Store the output of the `--yes` run. Each entry has the `branch` and the
`commit`. If a branch was removed by mistake, the commit can be recovered for as
long as git keeps it. Recreate the branch from the saved commit:

```sh
git branch <branch> <commit>
```

Run the command from a clone that has the commit, or fetch it first. Keep the
file out of the repository; it names review branches and commits, not secrets.

## Recovery list

Keep every `prune-branches --yes` output for at least as long as the
`store.review_retention_seconds` of your reviews. Without it, a branch
that was removed by mistake cannot be matched to its review.

## Do not clean up by hand

Do not delete review branches with `git push --delete` or `git branch -D` on
the review host while `serve` runs. The command checks that no running review
uses a branch; a manual delete does not. Use `prune-branches`, or
`pohunek session rm` for a session that `cleanup_manual` names.

## Related pages

- [Troubleshooting](troubleshooting.md#session-start-failures)
- [Retry and recovery](retry-and-recovery.md)
- [CLI reference](../../cli.md#maintenance)
- [State and recovery](../concepts/state-and-recovery.md)
