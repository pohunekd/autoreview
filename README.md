# Šafář review

Policy-driven AI pull request review on top of [pohunek](https://github.com/zajca/pohunek) sessions.
Šafář review is the product name; the repository, the binary and the command are
`autoreview`.

> **Status: 0.6.0.** autoreview 0.6.0 is the current release. It ships static
> Linux binaries for `x86_64` and `aarch64` as release archives, each with a
> `.sha256` file and a build provenance attestation, and an ad-hoc signed
> archive for Apple Silicon Macs with `scripts/setup` on macOS (#513); see
> [install](docs/install.md). The `main` branch holds changes after 0.6.0, and
> the documentation describes it.
> The [Rough plan](#rough-plan) table is the roadmap; a part that is not available
> names its open issue.

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
- **Notifications** - action / digest / log levels routed per level to Matrix
  rooms, webhooks (Slack, Discord, JSON) or only the log, single events
  optionally to their own route, plus the review queue.

## Install with an agent

Paste this prompt into a coding agent on the machine that runs autoreview; it
follows the install pages and asks before every change.

```text
Install autoreview (https://github.com/pohunekd/autoreview) on this machine.

First take the latest release tag <tag> (`gh release view --repo
pohunekd/autoreview --json tagName --jq .tagName`, or the releases page) and
use it below, never main. Read these first; they are the source of truth:
- https://raw.githubusercontent.com/pohunekd/autoreview/<tag>/docs/install.md
- https://raw.githubusercontent.com/pohunekd/autoreview/<tag>/docs/knowledge/guides/getting-started.md
- https://raw.githubusercontent.com/pohunekd/autoreview/<tag>/docs/knowledge/safety/secrets.md
- https://raw.githubusercontent.com/pohunekd/autoreview/<tag>/docs/knowledge/safety/untrusted-authors.md
- for pohunek on a server or another review host: https://pohunek.dev/install-prompt.txt

Rules:
1. Detect the OS, CPU and C library. autoreview runs on Linux x86_64 or
   aarch64, and on macOS 14 or newer on Apple Silicon from the first release
   with an `aarch64-apple-darwin` archive. `scripts/setup` needs Linux x86_64,
   Linux aarch64 with pohunek and its runtime packages in place, or such a Mac
   with bash 4 or newer from Homebrew; a Mac runs Claude reviewers only, Codex
   needs a Linux review host. For anything else, stop and tell me.
2. Before changing anything, ask me: one machine as my own user
   (`scripts/setup`, the only path on a Mac) or a Linux server (install page,
   path b with its systemd units); the configuration directory; the GitHub
   login autoreview acts as
   and its token kind (fine-grained for one owner, or classic); the
   repositories or organisation; Claude Code, Codex or both; the review
   hosts; where notifications go (setup asks for a Matrix room).
   Then show me the plan and wait for my approval.
3. Start with comments only: the `single-repo-comment` example, or
   `public-repositories` when people outside my team open pull requests, with
   the reviewers set to my agents. Set `on_findings: comment` and
   `on_clean: comment` in every rule (`public-repositories` ships
   `request_changes`) until I ask for more, and `trusted: any` only where no
   outside author can open a pull request.
4. With setup, run `git clone --branch <tag> --depth 1
   https://github.com/pohunekd/autoreview`. If the top-level
   `AUTOREVIEW_VERSION="X.Y.Z"` line near the start of its `scripts/setup` is
   not <tag> without the `v`, stop and tell me. Then run
   `scripts/setup --dry-run --dir <dir>` with my answers as the flags getting
   started lists. It needs a terminal, so give me the command without
   `--dry-run` to run in mine; never pipe answers into it or fake a terminal.
   Its first run stops after writing the configuration and the empty secrets
   file; I run it again after rules 6 and 7. Stop on exit code 5.
5. On a server, check the archive of <tag> with `sha256sum --check` and
   `gh attestation verify <archive> --repo pohunekd/autoreview
   --signer-workflow pohunekd/autoreview/.github/workflows/release.yml
   --source-ref refs/tags/<tag> --deny-self-hosted-runners`. Stop on any
   mismatch. Then follow path b and `autoreview init` as shown there.
6. Never read, print or store a token value; pass only the secrets file's
   path with `--env-file`. Tell me which `AUTOREVIEW_*` lines to write into it
   with an editor: with setup `$XDG_CONFIG_HOME/autoreview/autoreview.env`
   (`~/.config` when unset; 0600), on
   a server `sudoedit /etc/autoreview/autoreview.env` (owner autoreview, 0600).
7. Show me each policy.yaml edit (match, trigger, authors) as a diff first.
   Run `pohunek project add` and `autoreview profiles install` only after I
   approve. Never pass --accept-runtime-loss or --replace.
8. Verify with `autoreview validate --check-credentials` and `autoreview
   candidates` as the service user with its `--config` and `--env-file`,
   `systemctl status autoreview.service autoreview-queue.timer` (`--user`
   with setup) or, on a Mac, `scripts/macos-check service`, and `pohunek
   doctor --json`. Report every warning and fix only
   what I approve. End with the versions, paths, open warnings, next steps.
```

## Documentation

The documentation describes autoreview on `main` after the 0.6.0 release; a
part that is not available names its open issue.

- [Features](docs/features.md) - what it does and a map of every page
- [Getting started](docs/knowledge/guides/getting-started.md): `scripts/setup`
  sets up one Linux machine or one Mac with Apple Silicon (from 0.6.0 on,
  #513)
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
(`deploy/install.sh`, from 0.4.0 on).

## Rough plan

| Phase | Goal | Done when |
|---|---|---|
| 1. Extract and decouple | Fresh history, rename, move every personal or organisation-specific value into configuration, `Notifier` interface | Tests pass; `validate` accepts an example policy with no Matrix configured |
| 2. Adapters and installer | Claude and Codex adapters with one result contract, pohunek profile generator, release binaries, container image, systemd unit, `autoreview init` | A clean host goes from `init` to a published review by following the docs |
| 3. Safe defaults for public repositories | Author/fork-aware rules, `base` checkout for untrusted authors, threat model, pohunek version check | A fork pull request from an author the rule does not trust cannot be auto-approved and is never reviewed on a head checkout |
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
- `gh` CLI and one GitHub token, or one per owner or repository
- Claude Code and/or Codex CLI on the review hosts
- Linux on `x86_64` or `aarch64`; on one machine `scripts/setup` writes user
  units ([getting started](docs/knowledge/guides/getting-started.md)), and the
  installer of the install page writes the system units of a server user (from
  0.4.0 on; a container image is not available, see #40)
- Or macOS 14 or newer on Apple Silicon, from 0.6.0 on (#513):
  `scripts/setup` writes launchd agents of your user; Codex reviewers
  need a Linux review host

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
  owner or per repository, and a different account per owner if wanted.
  Failing or expiring tokens are reported, and a failing one stops only its
  repositories.
- Claude Code and Codex reviewers share one result contract, with the blocking
  flag of each finding and the verdicts on prior findings.
- Clean reviews that wait for a human are approved through the CLI or by an
  MCP client with the `write` scope (`autoreview mcp`); approval by GitHub
  comment and a GitHub App identity come after v1 (#46, #47).
- Untrusted authors are a policy class of their own and are never
  auto-approved.
- Distributed as static Linux release archives from 0.1.0 on, with systemd
  units and an installer from 0.4.0 on; a container image is not available (#40).
  An ad-hoc signed, attested Apple Silicon archive comes with 0.6.0 (#513); it
  is not notarized.
- MIT license.
