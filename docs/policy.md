---
type: Reference
id: reference/policy
title: Policy reference
description: Every key of policy.yaml - reviewers, pipelines, repository rules, author classes, approval, notes hand-off, draft handling and notification levels - with the rules autoreview validates at startup.
source_kind: manual
intents: [setup, project, help]
---

# Policy reference

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

The policy decides **what** is reviewed, **by whom**, and **what happens with
the result**. It lives in its own file, `policy.yaml`, next to the prompt files
it references. Technical settings (GitHub account, pohunek hosts, timeouts,
notifier backends) live in [config.yaml](configuration.md).

autoreview loads and validates the whole policy at startup and on
`autoreview validate`. Any error stops the service with a message that names
the key; nothing is guessed or defaulted. Every review copies the rule it was
dispatched with, so an edited policy applies from the next review on and never
changes a review in flight.

## A complete example

```yaml
version: 1

reviewers:
  opus:
    protocol: claude-structured
    agent_profile: autoreview-opus
    label: Opus
    finding_prefix: O
  sonnet:
    protocol: claude-structured
    agent_profile: autoreview-sonnet
    label: Sonnet
    finding_prefix: S
  codex:
    protocol: codex-envelope
    agent_profile: autoreview-codex
    label: Codex
    finding_prefix: X

pipelines:
  core:
    reviewers: [opus, sonnet]
    # A re-review settles earlier blocking findings, which needs the stronger model.
    incremental_reviewers: [opus]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]
    verifier:
      reviewer: opus
      prompt: prompts/verifier.md
      escalate_severities: [critical]
      escalate_count: 3
  light:
    reviewers: [codex]
    incremental_reviewers: [codex]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]

repositories:
  - match: zajca/pohunek
    trigger: review-requested
    pipeline: core
    on_findings: request_changes
    on_clean: await
    authors:
      trusted: any
  - match: pohunekd/*
    trigger: label:ai:review
    pipeline: light
    on_findings: request_changes
    on_clean: approve
    authors:
      trusted:
        permission: write
      untrusted:
        pipeline: light
        on_findings: comment
        on_clean: comment

unmatched: skip

ready_draft_authors: [devin-ai-integration]

approval:
  timeout_hours: 168

notifications:
  approval_requested: action
  handoff_reminder: action
  findings_on_own_pr: action
  credential_invalid: action
  credential_expiring: action
  review_stopped: action
  cleanup_manual: action
  command_failed: action
  approved: digest
  changes_requested: digest
  commented: digest
  approval_expired: digest
  started: log
  retry_scheduled: log
  superseded: log
```

The rules above span two owners, `zajca` and `pohunekd`; each is served by a
[credential](configuration.md#credentials) in `config.yaml`, which may be a
different account per owner.

## Top-level keys

| Key | Required | Value |
|---|---|---|
| `version` | yes | `1` |
| `reviewers` | yes | Map of reviewer id to [reviewer](#reviewers) |
| `pipelines` | yes | Map of pipeline name to [pipeline](#pipelines) |
| `repositories` | yes | List of 1-200 [repository rules](#repository-rules) |
| `unmatched` | yes | `skip`: a pull request no rule matches is never reviewed |
| `ready_draft_authors` | no | Lowercase GitHub logins of coding agents whose drafts are marked ready, see [drafts](#drafts-of-coding-agents) |
| `approval` | yes | `timeout_hours`: 1-720, how long a clean review waits for a human approval |
| `notes_handoff` | exactly when a rule sets `on_notes` | `reminder_hours`: 1-720 |
| `notifications` | yes | A level for every [event](#notifications) |

The file is at most 256 KiB, holds exactly one YAML document, and unknown keys
are an error.

## Reviewers

A reviewer is one coding agent run in its own pohunek session.

| Key | Value |
|---|---|
| id (map key) | `[a-z0-9][a-z0-9_-]{0,31}`; `verifier` is reserved |
| `protocol` | `claude-structured` or `codex-envelope`, see [reviewers and adapters](knowledge/concepts/reviewers-and-adapters.md) |
| `agent_profile` | The pohunek agent profile that runs the adapter, created by `autoreview profiles install` |
| `label` | Name shown in the review body, one line, at most 64 characters |
| `finding_prefix` | One uppercase letter other than `P` (reserved for prior findings published again), unique across reviewers. A reviewer numbers its findings `C001`, `C002`; the published id takes the prefix instead of `C`, so Opus's `C002` is published as `O002` |

## Pipelines

A pipeline says how a pull request is reviewed.

| Key | Value |
|---|---|
| name (map key) | `[a-z0-9][a-z0-9_-]{0,31}` |
| `reviewers` | 1-4 reviewer ids, all with the same protocol (the prompt is written for one request and result contract) |
| `incremental_reviewers` | 1-4 reviewer ids from `reviewers`; they run when only the new commits are reviewed |
| `prompt` | Relative path of the review prompt, inside the policy directory, non-empty UTF-8, at most 64 KiB |
| `checkout` | `base`: the worktree starts at the base branch tip, so repository guidance is read from trusted content; the reviewer reads the change through git (`git diff <base>...<head>`, `git show <head>:<path>`) without checking the head out. `head`: the worktree starts at `refs/pull/<n>/head` |
| `project_sources` | Which pohunek projects may serve the repository: `manual` (registered with `pohunek project add`), `auto` (discovered), or both |
| `verifier` | Optional, see below |

### Verifier

The verifier re-checks every blocking finding (and every prior finding the
reviewers call answered) before anything is published. See
[verification](knowledge/concepts/verification.md).

| Key | Value |
|---|---|
| `reviewer` | A reviewer id with protocol `claude-structured` (only that protocol enforces the verdict schema) |
| `prompt` | Relative path of the verifier prompt |
| `escalate_severities` | `critical`, `high`, or both, each once |
| `escalate_count` | At least 1 |

Both escalation keys are required on every verifier, including one used by a
rule that waits for a human anyway.

On a rule that approves clean reviews without a human (`on_clean: approve`),
the approval turns into a wait for a human when the verifier demoted a blocking
finding of an `escalate_severities` severity, or `escalate_count` blocking
findings.

## Repository rules

Rules are evaluated top to bottom and the **first match wins**. Matching looks
at the repository only, never at the trigger: each repository has exactly one
rule, and a pull request that does not fire that rule's trigger is not
reviewed, even if a later rule with another trigger would cover it. A rule
that an earlier rule already covers can therefore never apply and is rejected
(`unreachable, pohunekd/* matches first`), whatever its trigger. Put specific
repositories before their owner wildcard.

| Key | Value |
|---|---|
| `match` | Lowercase `owner/repository` or `owner/*` |
| `trigger` | `review-requested` (the pull request requests a review from the login of the [credential](configuration.md#credentials) that serves the repository) or `label:<name>` (`[A-Za-z0-9][A-Za-z0-9:._-]{0,49}`) |
| `pipeline` | A pipeline name |
| `on_findings` | What a review with blocking findings publishes: `request_changes` or `comment` |
| `on_clean` | What a review without blocking findings does: `approve`, `await` (wait for a human approval) or `comment` |
| `on_notes` | Optional. `comment`: a review whose findings are all notes goes to the author as a comment first, see [notes hand-off](knowledge/concepts/decisions.md#notes-hand-off). Pull requests authored by bots skip the hand-off and wait for approval directly |
| `authors` | Required. Who counts as a trusted author and what an untrusted author gets, see below |

Validation rules:

- `on_clean: approve` needs `incremental_reviewers` equal to `reviewers`, the
  same ids in the same order. Nobody
  confirms an automatic approval, so a re-review must not rest on fewer
  reviewers than the full review.
- `on_notes: comment` needs `on_clean: await` and `trigger: review-requested`:
  the author's renewed review request ends the hand-off, and only a human
  approval may follow it.

Whatever the rule says, a pull request **authored by the reviewing account**
(the login of the repository's credential) gets a comment: GitHub lets no author approve or request changes on their own
pull request. Findings on it raise the `findings_on_own_pr` event.

### Authors

Every rule states who it trusts. There is no implicit default: a rule without
`authors` is an error, so a public repository cannot be opened to strangers by
omission. See [untrusted authors](knowledge/safety/untrusted-authors.md).

```yaml
authors:
  trusted: any                 # every author is trusted; no untrusted section allowed
```

```yaml
authors:
  trusted:
    logins: [zajca]            # these logins are trusted
    permission: write          # and anyone with at least this repository permission
  untrusted: skip              # everyone else is not reviewed
```

```yaml
authors:
  trusted:
    permission: maintain
  untrusted:                   # everyone else is reviewed like this
    pipeline: light            # its checkout must be base
    on_findings: comment
    on_clean: comment
```

| Key | Value |
|---|---|
| `trusted` | `any`, or a mapping with `logins` (lowercase logins; a GitHub App author is written as its plain login without `app/` or `[bot]`, as in `ready_draft_authors`), `permission` (`write`, `maintain` or `admin`), or both. An author matching either is trusted |
| `untrusted` | Required unless `trusted: any`. `skip`, or a mapping with `pipeline`, `on_findings` and `on_clean` |

Rules for the untrusted section:

- Its pipeline must use `checkout: base`, so code from the pull request never
  becomes the reviewer's working tree.
- `on_clean` may be `comment` or `await`, never `approve`; `on_notes` is not
  allowed.
- A pull request from a fork is untrusted unless its author is listed in
  `logins`; repository permission alone does not make a fork trusted.
- The author class is resolved when a review is dispatched and stored with it.

## Drafts of coding agents

Some coding agents open pull requests as drafts and never mark them ready.
GitHub does not count a draft as asking for review, so it would never be
reviewed. A draft by a login in `ready_draft_authors` that requests a review
from the repository's credential login is marked ready for review, then reviewed like any other
pull request. GitHub search names an app author with the `app/` prefix;
write the plain login here.

## Notifications

Every event gets one of three levels. Where each level goes is configured in
[config.yaml](configuration.md#notify).

| Level | Meaning |
|---|---|
| `action` | Someone has to act; the notifier mentions the configured person |
| `digest` | Quiet record of a result |
| `log` | Muted; only the structured log |

| Event | When |
|---|---|
| `approval_requested` | A clean review waits for a human approval |
| `handoff_reminder` | Notes handed to an author got no renewed review request within `notes_handoff.reminder_hours` |
| `findings_on_own_pr` | A review of the reviewing account's own pull request has blocking findings |
| `credential_invalid` | A GitHub token was rejected, expired or belongs to another login; its repositories are not reviewed until the token is replaced and the service restarted |
| `credential_expiring` | A GitHub token expires within `github.expiry_warning_days`; raised once a day |
| `review_stopped` | A review failed after its retries, or an approved review could not be published |
| `cleanup_manual` | A reviewer session could not be removed and needs a manual `pohunek session rm` |
| `command_failed` | An operator command (approve, retry) failed |
| `approved` | An approval was published |
| `changes_requested` | A request for changes was published |
| `commented` | A comment review was published |
| `approval_expired` | A wait for approval passed `approval.timeout_hours` |
| `started` | A review started |
| `retry_scheduled` | A failed review step is retried later |
| `superseded` | A newer commit replaced a review, or the pull request stopped asking for one (review request or label removed, closed, back to draft) |

A `digest` or `log` message the notifier cannot deliver is dropped with a
warning. An `action` message that cannot be delivered fails the step, because
nobody would learn that they have to act.

## Prompts

Prompt paths are relative to the policy file and must stay inside its
directory. autoreview appends a trusted request context (repository, number,
head SHA, reviewer, review scope, prior findings) after the prompt; the pull
request content itself is always untrusted input. See
[writing prompts](knowledge/guides/writing-prompts.md).

## Validation errors

`autoreview validate` prints one JSON object. The most common errors:

| Message | Fix |
|---|---|
| `unreachable, owner/* matches first` | Move the specific rule above the wildcard |
| `on_clean approve needs incremental_reviewers equal to reviewers` | Use the same list, or `on_clean: await` |
| `on_notes comment needs on_clean await and trigger review-requested` | Change the rule, or drop `on_notes` |
| `verifier must use protocol claude-structured` | Point the verifier at a Claude reviewer |
| `all reviewers must use the same protocol` | Split the pipeline |
| `notifications: event X needs a level` | List every event |
| `needs notes_handoff.reminder_hours because a rule sets on_notes comment` | Add the section, or drop `on_notes` |
| `notes_handoff is set but no rule sets on_notes comment` | Remove the section, or set `on_notes` on a rule |
| `authors is required` | State who the rule trusts |
| `untrusted pipeline must use checkout base` | Use a base-checkout pipeline for untrusted authors |
