# Examples

Each directory is one scenario: a `policy.yaml`, a `README.md` that says what
it shows, and any prompt files it needs beyond the two default prompts in
[`prompts/`](../prompts/). [config.yaml](config.yaml) is the complete
configuration shared by all examples.

> **Status: 0.2.0.** These examples are written for autoreview 0.2.0.

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
autoreview init --dir <directory> --login <login> --pohunek-host <host> --example <name>
autoreview --config <directory>/config.yaml validate
```

`init` writes `config.yaml`, the example's `policy.yaml`, the two default
prompts `prompts/generic.md` and `prompts/verifier.md`, and the prompts the
example adds (see the [CLI reference](../docs/cli.md#autoreview-init)). It sets
`policy_file`, the `login` of the credential and `pohunek.hosts`. Set
`store.path`, and the homeserver, room and mentioned user of the placeholder
`matrix` backend in `notify` (see the
[notifiers guide](../docs/knowledge/guides/notifiers.md)). The README of each
example shows the manual copy instead, which also needs `policy_file`, the
`login` and `pohunek.hosts` set by hand.

## Shared choices

- Every example lists all notification events. Levels follow the reference
  defaults. Every route and the review queue need a `matrix` backend until M3,
  so `config.yaml` routes them to one backend with placeholder values.
- CI loads every example policy with `config.yaml` through the same loader as
  `validate` (`every_example_policy_validates_with_the_example_config`), with
  the prompts each README copies.
- `unmatched: skip` is the only value. A pull request no rule matches is never reviewed.
- Each `authors` section states who is trusted. No example relies on an implicit default.
- Examples use `checkout: base` for every pipeline, so reviewers read repository
  guidance from trusted content.
