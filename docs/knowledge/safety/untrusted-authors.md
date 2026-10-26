---
type: Safety
id: safety/untrusted-authors
title: Untrusted authors
description: How the authors block of a repository rule decides which pull requests are trusted, what an untrusted author gets, how forks are handled, and examples for private, team and public repositories.
source_kind: manual
intents: [setup, help]
---

# Untrusted authors

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

Every repository rule must say who it trusts. The `authors` block splits pull
request authors into two classes. A trusted author gets the rule's normal
outcomes, including approval. An untrusted author gets the section the rule
names, or nothing. The keys are in the [policy reference](../../policy.md#authors).

There is no default. A rule without `authors` is an error, so a public
repository cannot be opened to strangers by leaving the key out.

## The two classes

```yaml
authors:
  trusted:
    logins: [zajca]          # listed logins are trusted
    permission: write        # and anyone with at least this repository permission
  untrusted: skip            # everyone else is not reviewed
```

- `trusted` is `any`, or a mapping with `logins`, `permission`, or both. An author
  matching either one is trusted. `permission` takes `write`, `maintain` or `admin`.
- `trusted: any` means every author is trusted. Such a rule has no `untrusted`
  section; adding one is an error.
- `untrusted` is required unless `trusted: any`. It is `skip`, or a mapping with
  `pipeline`, `on_findings` and `on_clean`.

With `skip`, a pull request from an untrusted author is not reviewed.

## What an untrusted author gets

```yaml
authors:
  trusted:
    permission: maintain
  untrusted:
    pipeline: light            # a pipeline whose checkout is base
    on_findings: comment
    on_clean: comment
```

Validation enforces these rules, and the errors name them:

- **The pipeline must use `checkout: base`.** Otherwise the error is
  `untrusted pipeline must use checkout base`. The reviewer's worktree starts at
  the base branch tip, so code from the pull request never becomes the working tree.
- **`on_clean` is `comment` or `await`, never `approve`.** An untrusted author
  can never get an automatic approval. With `await`, a human may still approve
  the head after reading it, which is the intended way to accept such a change.
- **`on_notes` is not allowed** in an untrusted section.
- **`on_findings` is `request_changes` or `comment`.** Use `comment` when an
  automated run must not put a blocking state on an outside pull request.

The protection that matters most is the first one. `checkout: base` means the
reviewer reads the repository guidance (`AGENTS.md`, `CLAUDE.md`, `docs/`) from
the trusted base commit, not from the head the author chose. The pull request
diff is still read, but as data. See [prompt injection](prompt-injection.md).

## Forks

A pull request from a fork is untrusted unless its author is listed in `logins`.
Repository permission does not make a fork trusted: a maintainer with `write` on
the base repository who opens a pull request from a fork is untrusted unless the
login is listed too.

## When the class is decided

The author class is resolved when a review is dispatched, and it is stored with
that review, like the rule itself. A change of the author's permission or of
the policy applies to the next review, never to one in flight.

## Never approved automatically

Automatic approval is limited to trusted authors on rules with `on_clean: approve`.
That is the only path that can satisfy branch protection without a human. An
untrusted author's clean review ends in a comment, or in a wait for a human
(`await`) if the rule uses it. A pull request authored by the reviewing account
(the login of the credential that serves its repository) always gets a comment, whatever the rule says.

The engine's `approve` command and the approval wait are still available for
untrusted pull requests with `on_clean: await`. The human approving checks the
exact head SHA first.

## Examples

### Private repository

Only the team can open pull requests. Trust everyone with write access:

```yaml
repositories:
  - match: zajca/internal-tools
    trigger: review-requested
    pipeline: core
    on_findings: request_changes
    on_clean: await
    authors:
      trusted:
        permission: write
```

No `untrusted` section is needed when every author is trusted by permission.
Use `trusted: any` only when the repository really has no outside contributors.

### Public repository with a team

Maintainers have `maintain`; contributors open pull requests from forks.
Maintainers are reviewed normally, forks get comments:

```yaml
repositories:
  - match: pohunekd/pohunek-companion
    trigger: label:ai:review
    pipeline: core
    on_findings: request_changes
    on_clean: await
    authors:
      trusted:
        permission: maintain
      untrusted:
        pipeline: light          # checkout: base
        on_findings: comment
        on_clean: comment
```

A fork contributor who is a regular and trusted by the team can be listed in
`logins`, so their pull requests use the trusted path.

### Public repository with strangers

Anyone can open a pull request. Trust only the maintainers by login, skip
everyone else, and never auto-approve:

```yaml
repositories:
  - match: pohunekd/example-library
    trigger: label:ai:review
    pipeline: light
    on_findings: request_changes
    on_clean: await
    authors:
      trusted:
        logins: [maintainer-one, maintainer-two]
      untrusted: skip
```

The label trigger means a maintainer decides which pull request is reviewed.
That also limits who can spend model time. If you want strangers reviewed,
replace `skip` with the mapping from the previous example, and keep
`checkout: base` in its pipeline.

## Separate review host

Untrusted reviews should run on a review host that holds no other secrets. v0.1
selects the host per repository, not per author:

- `pohunek.route_overrides` pins one repository (`owner/repo`) to one host. All
  its pull requests, from trusted and untrusted authors, run there.
- Registering a repository only on one review host has the same effect.
- Choosing a host per author is not in v0.1. Do not split one repository's
  authors across two autoreview instances; the two would process the same pull
  requests.

Plan the separation per repository: a public repository with strangers gets a
dedicated review host, and a private repository can share the trusted host.

## Related pages

- [Threat model](threat-model.md)
- [Prompt injection](prompt-injection.md)
- [Policy reference, authors](../../policy.md#authors)
- [Public repositories guide](../guides/public-repositories.md)
- [Decisions](../concepts/decisions.md)
