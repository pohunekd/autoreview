# autoreview

Policy-driven AI pull request review on top of [pohunek](https://github.com/zajca/pohunek) sessions.

> **Status: 0.2.0.** autoreview 0.2.0 is the current release. It ships static
> Linux binaries for `x86_64` and `aarch64` as release archives, each with a
> `.sha256` file and a build provenance attestation; see [install](docs/install.md).
> It does not implement the whole v0.1 design yet. The [Rough plan](#rough-plan)
> table is the roadmap and its open items are tracked in GitHub issues; every
> other part that is still planned names its open issue.

## What it does

A long-running service watches GitHub for pull requests that ask for a review,
runs one or more coding agents (Claude, Codex) as read-only reviewers inside
pohunek sessions on your own machines, verifies their findings, and publishes
one GitHub review with inline comments.

- **Discovery** - a pull request becomes a candidate through a review request
  or a label, filtered by a repository policy.
- **Pipelines** - each matched repository runs a pipeline: which reviewers,
  which prompt, base or head checkout, and an optional verifier that
  re-checks findings before anything is published.
- **Decisions** - per repository: request changes on blocking findings,
  approve or wait for a human on a clean review, hand non-blocking notes to
  the author as a comment.
- **Incremental re-review** - a new push is reviewed against the previously
  published head; earlier findings are settled as fixed, still open or
  answered, taking the PR discussion into account, by Claude and Codex
  reviewers alike.
- **Durable state** - SQLite store, retries with backoff, approval deadlines,
  session cleanup, an hourly review-queue digest.
- **Notifications** - action / digest / log levels routed to Matrix rooms per
  level, single events optionally to their own room, plus the review queue;
  `none` and webhook backends are planned in #29.

## Documentation

The documentation describes autoreview 0.2.0; parts marked planned are not in
0.2.0.

- [Features](docs/features.md) - what it does and a map of every page
- [Getting started](docs/knowledge/guides/getting-started.md): `scripts/setup`
  (after 0.2.0) sets up one Linux machine
- [Quickstart](docs/knowledge/guides/quickstart.md) and
  [use cases](docs/knowledge/guides/index.md), each with a complete policy in
  [examples/](examples/)
- Reference: [policy](docs/policy.md), [configuration](docs/configuration.md),
  [CLI](docs/cli.md), [install](docs/install.md)

## Architecture

```
GitHub (gh) --> discovery --> policy match --> review workflow (SQLite)
                                                  |
                         pohunek session per reviewer (Claude / Codex adapter)
                                                  |
                                   parse -> verify -> publish review -> notify
```

The engine needs three parts to be useful: the engine itself (Rust), the
reviewer adapters that turn a pohunek agent session into one structured,
read-only review run, and an installer that creates the pohunek agent profiles
(`autoreview profiles install`) and the service units with their installer
(`deploy/install.sh`, after 0.3.0).

## Rough plan

| Phase | Goal | Done when |
|---|---|---|
| 1. Extract and decouple | Fresh history, rename, move every personal or organisation-specific value into configuration, `Notifier` interface | Tests pass; `validate` accepts an example policy with no Matrix configured |
| 2. Adapters and installer | Claude and Codex adapters with one result contract, pohunek profile generator, release binaries, container image, systemd unit, `autoreview init` | A clean host goes from `init` to a published review by following the docs |
| 3. Safe defaults for public repositories | Author/fork-aware rules, `base` checkout for untrusted authors, threat model, pohunek version check | A fork pull request whose author is not listed in `logins` cannot be auto-approved and never runs head code with setup hooks |
| 4. Migrate the original deployment | The private deployment consumes a release; legacy review markers and state still read | Incremental reviews continue on pull requests reviewed before the migration |
| 5. Examples, docs, dogfooding | One small policy per scenario over the pohunek repositories, validated in CI; live on `pohunekd/*` | Every example passes `validate` in CI; the org's pull requests are reviewed by it |
| 6. After v1 | Approval by a GitHub comment command, GitHub App identity, more notifier backends | Decided one by one when started |

### Examples

Each example is a short policy file with a README under
[examples/](examples/), all over pohunek and related repositories. The last two
scenarios are guides without a policy of their own:

1. `single-repo-comment` - one repository, label trigger, one Codex reviewer, always comment
2. `solo-maintainer` - own pull requests get a comment, others request changes or approve
3. `human-approval` - two reviewers plus verifier, a human approves clean reviews
4. `auto-approve` - website repository, one reviewer, clean reviews approved
5. `org-wildcard` - `pohunekd/*` with per-repository pipelines and prompts
6. `notes-handoff` - non-blocking notes go to the author first
7. `coding-agent-drafts` - drafts of coding agents marked ready and reviewed
8. `public-repositories` - external authors: label gate, base checkout, comment only
9. [`multihost`](docs/knowledge/guides/multihost.md) - several pohunek hosts and
   route overrides
10. [`notifiers`](docs/knowledge/guides/notifiers.md) - Matrix rooms per level or
    per event and the review queue

## Requirements

- pohunek daemon on one or more review hosts with the repositories registered
- `gh` CLI and one GitHub token (tokens per owner or repository are planned in #14 to #18)
- Claude Code and/or Codex CLI on the review hosts
- Linux on `x86_64` or `aarch64`; on one machine `scripts/setup` (after 0.2.0)
  writes user units ([getting started](docs/knowledge/guides/getting-started.md)), and the
  installer of the install page writes the system units of a server user (after
  0.3.0; the container image is planned in #40)

## Security model

Pull request content is untrusted. Reviewers run read-only and must not follow
instructions found in the diff, comments or repository files. Every rule says
which authors it trusts. An untrusted author is skipped or reviewed from a base
checkout, and is never auto-approved; the `public-repositories` example also
keeps untrusted authors behind a maintainer label.

## Decisions so far

- Binary `autoreview`; one YAML configuration file with every key required
  and written out in full by `autoreview init`; secrets only in `AUTOREVIEW_*`
  variables, from the environment or the file given with `--env-file`.
- GitHub tokens as a list of credentials: one token, fine-grained tokens per
  owner or per repository, and a different account per owner if wanted
  (planned in #14 to #18). Failing or expiring tokens are reported and stop only their
  repositories (planned in #19).
- Claude Code and Codex reviewers share one result contract, with the blocking
  flag of each finding and the verdicts on prior findings.
- Clean reviews that wait for a human are approved through the CLI, with an
  optional Hermes Agent integration (planned in #43); approval by GitHub comment and a GitHub
  App identity come after v1 (#46, #47).
- Untrusted authors are a policy class of their own and are never
  auto-approved.
- Distributed as static Linux release archives from 0.1.0 on, with systemd
  units and an installer after 0.3.0; a container image (planned in #40)
  follows.
- MIT license.
