---
type: Reference
id: reference/features
title: Features
description: What autoreview does - discovery, policy, reviewer pipelines on pohunek, verification, decisions, incremental re-reviews, approvals, notifications and durable state - and where each is documented.
source_kind: manual
intents: [setup, help]
---

# Features

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

autoreview is a small service that reviews GitHub pull requests with coding
agents you already run. It finds pull requests that ask for a review, runs one
or more agents (Claude Code, Codex) as read-only reviewers in
[pohunek](https://github.com/zajca/pohunek) sessions on your own machines,
checks their findings, and publishes one GitHub review with inline comments.
A policy file decides which repositories are reviewed, by which agents, and
what the result is allowed to do.

It does not run in GitHub Actions and does not send your code to a hosted
review service: the agents run in worktrees on hosts you control, with the
models and accounts you configure.

## What it does

**Finds the work.** A pull request becomes a candidate when it requests a
review from the service's GitHub account, or when it carries a label you
choose. Drafts of coding agents that never leave draft can be marked ready
automatically. A burst of pushes is reviewed once.
[How it works](knowledge/concepts/how-it-works.md)

**Follows a policy.** Rules map repositories (`owner/repo` or `owner/*`) to a
pipeline and to the outcomes the review may publish. The first matching rule
wins and a rule that can never match is an error.
[Policy reference](policy.md)

**Runs agents as reviewers.** A pipeline names one to four reviewers, the
prompt, and whether the worktree starts at the trusted base or the pull
request head. Every reviewer runs in its own pohunek session through an
adapter that enforces a read-only run and a structured result.
[Reviewers and adapters](knowledge/concepts/reviewers-and-adapters.md)

**Checks findings before publishing.** An optional verifier re-checks each
blocking finding against the code and turns the ones it cannot confirm into
notes, with the reason. Nothing is dropped silently.
[Verification](knowledge/concepts/verification.md)

**Decides what to publish.** Blocking findings request changes or comment.
A clean review approves, comments, or waits for a human. Non-blocking notes
can go to the author first. Your own pull requests always get a comment.
[Decisions](knowledge/concepts/decisions.md)

**Re-reviews only what changed.** After a push, the new commits are reviewed
against the last published head, and every earlier blocking finding is settled
as fixed, still open, or answered, taking the pull request discussion into
account. [Incremental reviews](knowledge/concepts/incremental-reviews.md)

**Keeps humans in charge where it matters.** A clean review can wait for your
approval, given from the CLI or a chat integration and bound to the exact head
commit. An approval policy can escalate to a human when the verifier demoted
serious findings. [Human approval](knowledge/guides/human-approval.md)

**Treats strangers differently.** Each rule states which authors are trusted.
Untrusted authors and forks are reviewed from the base checkout and are never
approved automatically. [Untrusted authors](knowledge/safety/untrusted-authors.md)

**Tells the right person.** Events have three levels (act now, digest, log)
routed to notifier backends: none, Matrix, or any webhook. An hourly queue
lists every pull request waiting on you or on its author.
[Notifications](knowledge/concepts/notifications.md)

**Uses the right account per repository.** Each owner or repository can have
its own token and even its own GitHub account; tokens never mix, a failing
token stops only its own repositories, and expiring tokens are reported in
time. [Configuration: credentials](configuration.md#credentials)

**Survives failure.** All state is in one SQLite file. Reviews resume after a
restart, transient failures are retried with backoff, and sessions it cannot
clean up are reported. [State and recovery](knowledge/concepts/state-and-recovery.md)

## Requirements

- A pohunek daemon on one or more review hosts, with the repositories
  registered as pohunek projects and the official Claude Code and/or Codex
  runtime packages installed.
- The GitHub CLI and one or more tokens: a single token, fine-grained tokens per
  owner or per repository, and a different account per owner if you want one
  (your own, or a machine user). [GitHub identity](knowledge/concepts/github-identity.md)
- Linux with systemd, a container runtime, or pohunek's workflow plugin store.

## Documentation map

| Start here | |
|---|---|
| [Install](install.md) | Binary, container, first configuration, first review |
| [Quickstart](knowledge/guides/quickstart.md) | One repository, one reviewer, comments only |
| [Use cases](knowledge/guides/index.md) | Pick a scenario and combine them |

| Reference | |
|---|---|
| [Policy](policy.md) | `policy.yaml` |
| [Configuration](configuration.md) | `config.yaml` and environment |
| [CLI](cli.md) | Commands and error codes |

| Concepts | |
|---|---|
| [How it works](knowledge/concepts/how-it-works.md) | The life of one review |
| [Reviewers and adapters](knowledge/concepts/reviewers-and-adapters.md) | Protocols, profiles, read-only runs |
| [Decisions](knowledge/concepts/decisions.md) | Outcomes, own pull requests, notes hand-off, approval waits |
| [Incremental reviews](knowledge/concepts/incremental-reviews.md) | Re-review scope and prior findings |
| [Verification](knowledge/concepts/verification.md) | The verifier and escalation |
| [Notifications](knowledge/concepts/notifications.md) | Events, levels, backends, review queue |
| [GitHub identity](knowledge/concepts/github-identity.md) | Which account posts, token scopes |
| [State and recovery](knowledge/concepts/state-and-recovery.md) | Store, retries, resume, sessions |

| Use cases | |
|---|---|
| [Comment on one repository](knowledge/guides/single-repo-comment.md) | Safest start |
| [Solo maintainer](knowledge/guides/solo-maintainer.md) | Your pull requests and contributors' |
| [Human approval](knowledge/guides/human-approval.md) | Two reviewers, verifier, you approve |
| [Automatic approval](knowledge/guides/auto-approve.md) | Low-risk repositories |
| [Organization rules](knowledge/guides/org-wildcard.md) | Wildcards with per-repository overrides |
| [Notes hand-off](knowledge/guides/notes-handoff.md) | Optional notes go to the author first |
| [Coding-agent drafts](knowledge/guides/coding-agent-drafts.md) | Review agent pull requests |
| [Public repositories](knowledge/guides/public-repositories.md) | Forks and strangers |
| [Several review hosts](knowledge/guides/multihost.md) | Spread reviewers, pin routes |
| [Notifiers](knowledge/guides/notifiers.md) | None, Matrix, webhooks |
| [Combining patterns](knowledge/guides/combining.md) | Recipes that layer the above |
| [Writing prompts](knowledge/guides/writing-prompts.md) | Review and verifier prompts |
| [Approve from chat with Hermes](knowledge/guides/hermes-approval.md) | Optional Hermes Agent integration |

| Operate | |
|---|---|
| [Troubleshooting](knowledge/runbooks/troubleshooting.md) | Error codes to fixes |
| [Retry and recovery](knowledge/runbooks/retry-and-recovery.md) | Stopped reviews, restarts |
| [Stale sessions and branches](knowledge/runbooks/stale-sessions-and-branches.md) | Cleanup on review hosts |
| [Upgrade and backup](knowledge/runbooks/upgrade-and-backup.md) | Releases, database |

| Safety | |
|---|---|
| [Threat model](knowledge/safety/threat-model.md) | What runs where, who controls what |
| [Untrusted authors](knowledge/safety/untrusted-authors.md) | Author classes and forks |
| [Prompt injection](knowledge/safety/prompt-injection.md) | Pull request content is data |
| [Secrets](knowledge/safety/secrets.md) | Tokens and reviewer environments |
