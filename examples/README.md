# Examples

Each directory is one scenario: a `policy.yaml`, a `README.md` that says what
it shows, and any prompt files it needs beyond the two default prompts in
[`prompts/`](../prompts/). [config.yaml](config.yaml) is the complete
configuration shared by all examples.

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
mkdir -p <directory>/prompts
cp examples/config.yaml <directory>/config.yaml
cp examples/<name>/policy.yaml <directory>/policy.yaml
cp prompts/generic.md prompts/verifier.md <directory>/prompts/
autoreview --config <directory>/config.yaml validate
```

In the copy of `config.yaml`, set `policy_file` to the absolute path of the
policy, the `login` of the credential, `pohunek.hosts`, `store.path`, and the
homeserver, room and mentioned user of the placeholder `matrix` backend in
`notify` (see the [notifiers guide](../docs/knowledge/guides/notifiers.md)). For examples with
extra prompt files, copy them into `<directory>/prompts/` as well (see the
README of `org-wildcard`). `autoreview init` is planned in #8.
Every example references the same two
default prompts, `prompts/generic.md` and `prompts/verifier.md`.

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
