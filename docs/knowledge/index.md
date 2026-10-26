# autoreview knowledge

Hand-written documentation for autoreview, in the same layout as the pohunek
knowledge bundle so pohunek.dev can render both.

> **Status: planned for v0.1.** autoreview is not released yet. These pages describe the design the first release implements; commands and keys may still change.

- **Concepts** explain how a review works:
  [how it works](concepts/how-it-works.md),
  [reviewers and adapters](concepts/reviewers-and-adapters.md),
  [decisions](concepts/decisions.md),
  [incremental reviews](concepts/incremental-reviews.md),
  [verification](concepts/verification.md),
  [notifications](concepts/notifications.md),
  [GitHub identity](concepts/github-identity.md) and
  [state and recovery](concepts/state-and-recovery.md).
- **Guides** are use cases with a complete policy each. Start with the
  [quickstart](guides/quickstart.md) and the [use case chooser](guides/index.md);
  [combining patterns](guides/combining.md) layers them in one policy.
- **Runbooks** fix things in operation:
  [troubleshooting](runbooks/troubleshooting.md),
  [retry and recovery](runbooks/retry-and-recovery.md),
  [stale sessions and branches](runbooks/stale-sessions-and-branches.md) and
  [upgrade and backup](runbooks/upgrade-and-backup.md).
- **Safety** pages state the trust model:
  [threat model](safety/threat-model.md),
  [untrusted authors](safety/untrusted-authors.md),
  [prompt injection](safety/prompt-injection.md) and
  [secrets](safety/secrets.md).

The reference pages are [policy](../policy.md),
[configuration](../configuration.md) and [CLI](../cli.md); the
[features](../features.md) page maps everything.
