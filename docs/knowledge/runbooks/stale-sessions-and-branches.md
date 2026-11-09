---
type: Runbook
id: runbook/stale-sessions-and-branches
title: Stale sessions and branches
description: Why review branches and pohunek sessions accumulate on the review hosts, and how to remove a session that autoreview could not remove.
source_kind: manual
intents: [debug, help]
---

# Stale sessions and branches

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0.

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

autoreview removes a reviewer's session, not its branch, and no autoreview
command removes review branches. A pruning command is planned in #42.

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

## Branches of running reviews

A running review works on its branch, and nothing checks a manual delete
against the running reviews. Do not delete review branches with
`git push --delete` or `git branch -D` on the review host while `serve` runs.

## Related pages

- [Troubleshooting](troubleshooting.md#session-start-failures)
- [Retry and recovery](retry-and-recovery.md)
- [CLI reference](../../cli.md#maintenance)
- [State and recovery](../concepts/state-and-recovery.md)
