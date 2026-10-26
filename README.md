# autoreview

Policy-driven AI pull request review on top of [pohunek](https://github.com/zajca/pohunek) sessions.

> **Status: planning.** This repository contains no code yet. The engine runs
> privately today and is being extracted into a reusable, configurable project.
> This README is the rough plan.

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
  answered, taking the PR discussion into account.
- **Durable state** - SQLite store, retries with backoff, approval deadlines,
  session cleanup, an hourly review-queue digest.
- **Notifications** - action / digest / log levels routed to one or more
  notifier backends (none, Matrix and webhook first; the layer is built for
  any channel).

## Documentation

The documentation is written ahead of the first release and describes the
design v0.1 implements.

- [Features](docs/features.md) - what it does and a map of every page
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

The engine needs three parts to be useful: the engine itself (Go), the
reviewer adapters that turn a pohunek agent session into one structured,
read-only review run, and an installer that creates the pohunek agent profiles
and the service unit.

## Rough plan

| Phase | Goal | Done when |
|---|---|---|
| 1. Extract and decouple | Fresh history, rename, move every personal or organisation-specific value into configuration, `Notifier` interface | Tests pass; `validate` accepts an example policy with no Matrix configured |
| 2. Adapters and installer | Claude and Codex adapters with one result contract, pohunek profile generator, release binaries, container image, systemd unit, workflow plugin, `autoreview init` | A clean host goes from `init` to a published review by following the docs |
| 3. Safe defaults for public repositories | Author/fork-aware rules, `base` checkout for untrusted authors, threat model, pohunek version check | A fork pull request cannot be auto-approved and never runs head code with setup hooks |
| 4. Migrate the original deployment | The private deployment consumes a release; legacy review markers and state still read | Incremental reviews continue on pull requests reviewed before the migration |
| 5. Examples, docs, dogfooding | One small policy per scenario over the pohunek repositories, validated in CI; live on `pohunekd/*` | Every example passes `validate` in CI; the org's pull requests are reviewed by it |
| 6. After v1 | Approval by a GitHub comment command, GitHub App identity, more notifier backends | Decided one by one when started |

### Planned examples

Each example is a short policy file with a README, all over pohunek and related
repositories:

1. `single-repo-comment` - one repository, label trigger, one Codex reviewer, always comment
2. `solo-maintainer` - own pull requests get a comment, others request changes or approve
3. `human-approval` - two reviewers plus verifier, a human approves clean reviews
4. `auto-approve` - website repository, one reviewer, clean reviews approved
5. `org-wildcard` - `pohunekd/*` with per-repository pipelines and prompts
6. `notes-handoff` - non-blocking notes go to the author first
7. `coding-agent-drafts` - drafts of coding agents marked ready and reviewed
8. `public-repositories` - external authors: label gate, base checkout, comment only
9. `multihost` - several pohunek hosts and route overrides
10. `notifiers` - none, Matrix and webhook side by side

## Requirements (planned)

- pohunek daemon on one or more review hosts with the repositories registered
- `gh` CLI and one or more GitHub tokens (per owner or repository, optionally a different account per owner)
- Claude Code and/or Codex CLI on the review hosts
- Linux with systemd, a container runtime, or pohunek's workflow plugin store

## Security model (planned)

Pull request content is untrusted. Reviewers run read-only and must not follow
instructions found in the diff, comments or repository files. For public
repositories the defaults keep untrusted authors behind a maintainer label,
review them from a base checkout, and never auto-approve them.

## Decisions so far

- Binary `autoreview`; one YAML configuration file with every key required
  and written out in full by `autoreview init`; secrets only in `AUTOREVIEW_*`
  environment variables.
- GitHub tokens as a list of credentials: one token, fine-grained tokens per
  owner or per repository, and a different account per owner if wanted.
  Failing or expiring tokens are reported and stop only their repositories.
- Claude Code and Codex reviewers share one result contract.
- Clean reviews that wait for a human are approved through the CLI, with an
  optional Hermes Agent integration; approval by GitHub comment and a GitHub
  App identity come after v1.
- Untrusted authors are a policy class of their own and are never
  auto-approved.
- Distributed as release binaries with systemd units, as a container image,
  and as a pohunek workflow plugin.
- MIT license.
