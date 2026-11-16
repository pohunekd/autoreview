---
type: Runbook
id: runbook/stale-sessions-and-branches
title: Stale sessions and branches
description: Why review branches and pohunek sessions accumulate on the review hosts, how to remove a session that autoreview could not remove, and how to remove the branches that no review needs.
source_kind: manual
intents: [debug, help]
---

# Stale sessions and branches

> **Status: after 0.4.0.** This page describes autoreview on `main` after the
> 0.4.0 release. A part that is not available names its open issue.

Every reviewer run happens in a pohunek session on a review host, and each
session works on its own review branch. Two upstream behaviours make these
pile up.

## Why branches accumulate

- **pohunek keeps a removed session's branch.** Removing a session with
  `pohunek session rm` does not delete its branch. Upstream issue
  [zajca/pohunek#1438](https://github.com/zajca/pohunek/issues/1438) tracks it.
- **Every start attempt has its own branch.** A base fetch that hits a locked
  ref ([zajca/pohunek#1437](https://github.com/zajca/pohunek/issues/1437)) is retried on
  a fresh branch. The branch name is
  `<session.branch_prefix><workflow>-<reviewer>-run<N>[-start<M>]`, so a single
  review can leave several branches behind.

autoreview removes a reviewer's session, not its branch. Remove the leftover branches
with `autoreview prune-branches`, described below.

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

[`autoreview prune-branches`](../../cli.md#autoreview-prune-branches) lists
the branches under `session.branch_prefix` that no review needs, and deletes them with
`--yes`. Run it on the machine that holds the store and the review host's repositories, as
the user of the service, while `serve` runs or not. The repositories must be owned by that
user, and a repository whose configuration names a `reference-transaction` hook is not
pruned ([details](../../cli.md#autoreview-prune-branches)):

```sh
autoreview prune-branches --git-binary /usr/bin/git                    # list only
autoreview prune-branches --git-binary /usr/bin/git --older-than-days 7 --yes
```

It never deletes a branch whose review is queued, running, awaiting or still being cleaned
up, a branch a pohunek session of the host works on, a branch checked out in a worktree, or
a symbolic ref.
Each deleted branch is printed with its commit; `git -C <path> branch <branch> <commit>`
restores it. pohunek has no command that deletes a branch on another machine, so a review
host whose repositories are elsewhere is skipped as `host_not_local`. Do not delete review
branches with `git push --delete` or `git branch -D`: nothing checks a manual delete against
the running reviews.

A failed deletion, or a `--host` host or repository that is skipped, ends the command with
status 1 and `branch_prune_incomplete`. The branches it did not delete stay; run it again
once the cause is fixed.

## Related pages

- [Troubleshooting](troubleshooting.md#session-start-failures)
- [Retry and recovery](retry-and-recovery.md)
- [CLI reference](../../cli.md#maintenance)
- [State and recovery](../concepts/state-and-recovery.md)
