---
type: Guide
id: guide/use-cases
title: Use cases
description: Choose the autoreview setup that fits your repositories - who approves, whether the repository is public, how many repositories share one policy - and see every scenario guide and the dimensions they combine.
source_kind: manual
intents: [setup, help]
---

# Use cases

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

Every scenario below is a policy file plus a config file. Start from the
closest scenario, copy its example, and change the repository names. The
examples live in [`examples/`](../../../examples/) and are the files
`autoreview init --example <name>` writes.

## Choose a scenario

Answer the questions in order. The first question that fits your situation
gives the guide to start from.

1. **Do you only want comments for now, with nobody approving anything?**
   Start with [single-repo-comment](single-repo-comment.md). It is the
   default example and the safest first run.
2. **Is the repository public, or can strangers open pull requests on it?**
   Read [public-repositories](public-repositories.md) first. Untrusted authors
   and forks are reviewed from the base checkout and never approved.
3. **Do you want a human to approve a clean review before anything is
   approved on GitHub?**
   Use [human-approval](human-approval.md). A clean review waits with
   `on_clean: await` until `autoreview approve` (or the optional chat
   integration) confirms the exact head.
4. **Is the repository low-risk enough that a clean review may approve by
   itself?**
   Use [auto-approve](auto-approve.md). Only trusted authors get an automatic
   approval, and the verifier can turn it into a wait for a human.
5. **Is this one repository, or several repositories of one owner?**
   One repository: stay with the guide from question 1, 3 or 4. Several
   repositories of one owner: use [org-wildcard](org-wildcard.md) and put the
   specific repositories above the `owner/*` rule.
6. **Do you want the author to get non-blocking notes before a review is
   requested again?**
   Use [notes-handoff](notes-handoff.md). It only works with
   `on_clean: await` and the `review-requested` trigger.
7. **Do coding agents (Devin, Copilot and similar) open your pull requests as
   drafts?**
   Use [coding-agent-drafts](coding-agent-drafts.md) to have their drafts
   marked ready for review, and to decide which of them are reviewed at all.
8. **Do you keep a solo repository and also review pull requests from
   contributors?**
   Use [solo-maintainer](solo-maintainer.md).
9. **Does a review need to run on more than one pohunek host?**
   Read [multihost](multihost.md).
10. **Do you need more than one scenario in the same policy?**
    Read [combining](combining.md) for recipes that layer them.

## Scenario guides

| Guide | Use it when | Outcomes | Trusted authors | Approval |
|---|---|---|---|---|
| [single-repo-comment](single-repo-comment.md) | You try autoreview on one repository | Comment only | Any | None |
| [solo-maintainer](solo-maintainer.md) | You own a repository and get contributions | Request changes or approve for you | Yours, others comment | Yours |
| [human-approval](human-approval.md) | A person decides on every clean review | Request changes, then wait | Write permission | Human |
| [auto-approve](auto-approve.md) | A low-risk repository may approve itself | Approve when clean | Write permission | None |
| [org-wildcard](org-wildcard.md) | Many repositories of one owner | Per repository | Per repository | Per repository |
| [notes-handoff](notes-handoff.md) | Notes go to the author before approval | Comment with notes first | Write permission | Human |
| [coding-agent-drafts](coding-agent-drafts.md) | Coding agents open pull requests as drafts | Ready and reviewed | Coding agent logins | Human |
| [public-repositories](public-repositories.md) | Strangers and forks open pull requests | Comment for untrusted authors | Named logins only | Human or none |

The table describes the typical setup. Each guide states the exact rules.

## Dimensions you combine

Every scenario is a choice along six independent dimensions. A policy picks
one value per dimension for each repository rule.

| Dimension | Values | Keys | Read |
|---|---|---|---|
| Trigger | Review requested from the bot, or a label | `trigger` | [policy](../../policy.md#repository-rules) |
| Pipeline | Reviewers, incremental reviewers, prompt, checkout, project sources, verifier | `pipelines` | [policy](../../policy.md#pipelines) |
| Outcomes | Blocking findings, clean review, notes | `on_findings`, `on_clean`, `on_notes` | [decisions](../concepts/decisions.md) |
| Authors | Trusted by login or permission, untrusted handling | `authors` | [untrusted authors](../safety/untrusted-authors.md) |
| Notifications | Level per event, routes to backends | `notifications`, `notify.routes` | [notifications](../concepts/notifications.md) |
| Hosts | pohunek hosts that run reviewers, route overrides | `pohunek.hosts`, `pohunek.route_overrides` | [multihost](multihost.md) |

Two dimensions are not in a policy: the GitHub accounts (one `login` per credential in
`github.credentials` of [config.yaml](../../configuration.md)) and the review prompt text
([writing prompts](writing-prompts.md)).

## Where to go next

- [Quickstart](quickstart.md) runs the first review on one repository.
- [Combining patterns](combining.md) lists recipes and the rules that
  interact when two scenarios share one policy.
- [Notifiers](notifiers.md) explains the notification backends.
- [Writing prompts](writing-prompts.md) explains what a prompt must and must
  not do.
- [Hermes approval](hermes-approval.md) is an optional chat integration for
  the approval step.
