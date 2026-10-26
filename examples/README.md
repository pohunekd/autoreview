# Examples

Each directory is one scenario: a `policy.yaml` that validates on its own, a
`README.md` that says what it shows, and any prompt files it needs beyond the
two that `autoreview init` writes. [config.yaml](config.yaml) is the complete
configuration shared by all examples; `autoreview init` writes its own copy with
your values.

> **Status: planned for v0.1.** autoreview is not released yet. These examples describe the design the first release implements.

| Example | Shows | Repositories | Outcome |
|---|---|---|---|
| [single-repo-comment](single-repo-comment/README.md) | Label trigger, one reviewer, comments only | `zajca/pohunek` | Comment |
| [solo-maintainer](solo-maintainer/README.md) | Own pull requests, trusted and untrusted authors | `zajca/pohunek`, `zajca/pohunek-work` | Request changes or approve; comment for others |
| [human-approval](human-approval/README.md) | Two reviewers, verifier with escalation, approval by a person | `zajca/pohunek` | Request changes; wait for approval |
| [auto-approve](auto-approve/README.md) | One reviewer, verifier, automatic approval | `pohunekd/pohunek.dev` | Request changes or approve |
| [org-wildcard](org-wildcard/README.md) | Specific rules before a wildcard, per-repository prompts | `pohunekd/pohunek-companion`, `pohunekd/pohunek-companion-android`, `pohunekd/*` | Per rule |
| [notes-handoff](notes-handoff/README.md) | Notes go to the author first | `pohunekd/pohunek-companion` | Comment, then wait for approval |
| [coding-agent-drafts](coding-agent-drafts/README.md) | Drafts of coding agents marked ready | `zajca/pohunek` | Wait for approval |
| [public-repositories](public-repositories/README.md) | Label gate, maintainers only, base checkout for others | `pohunekd/*`, `zajca/pohunek` | Comment; skip strangers |

## Use one

```sh
autoreview init --dir <directory> --login <github-login> --pohunek-host <host> --example <name>
autoreview --config <directory>/config.yaml validate
```

`init` writes `policy.yaml` from the named example and the default prompts. For
examples with extra prompt files, copy them into the policy directory as well
(see the README of `org-wildcard`). Every example references the same two
default prompts, `prompts/generic.md` and `prompts/verifier.md`.

## Shared choices

- Every example lists all notification events. Levels follow the reference
  defaults; `action` events have no notifier until you add one to `notify.routes`
  (see the [notifiers guide](../docs/knowledge/guides/notifiers.md)).
- `unmatched: skip` is the only value. A pull request no rule matches is never reviewed.
- Each `authors` section states who is trusted. No example relies on an implicit default.
- Examples use `checkout: base` for every pipeline, so reviewers read repository
  guidance from trusted content.
