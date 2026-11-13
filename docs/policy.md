---
type: Reference
id: reference/policy
title: Policy reference
description: Every key of policy.yaml - reviewers, pipelines, repository rules, author classes, approval, notes hand-off, draft handling and notification levels - with the rules autoreview validates at startup.
source_kind: manual
intents: [setup, project, help]
---

# Policy reference

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0.

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

The rules above span two owners, `zajca` and `pohunekd`; both are served by the
one [credential](configuration.md#credentials) in `config.yaml`. A credential
per owner passes `validate`; serving several is planned in #14 to #17.

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
| `agent_profile` | The pohunek agent profile that runs the adapter; it must exist on the review host; `autoreview profiles install` writes it there |
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
| `checkout` | `base`: the worktree starts at the base branch tip, so repository guidance is read from trusted content; the reviewer reads the change through git (`git diff <base>...<head>`, `git show <head>:<path>`) without checking the head out. Pohunek fetches only the base branch, so the head commit must already be in the repository; otherwise the Claude and Codex adapters stop the review with `review_head_unavailable` and nothing is published. `head`: the worktree starts at `refs/pull/<n>/head` |
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
| `match` | Lowercase `owner/repository` or `owner/*`. The owner is an organization or a user; an Enterprise Managed User is `handle_shortcode`, such as `mona-cat_octo/*` |
| `trigger` | `review-requested` (the pull request requests a review from the login of the [credential](configuration.md#credentials) that serves the repository) or `label:<name>` (`[A-Za-z0-9][A-Za-z0-9:._-]{0,49}`) |
| `pipeline` | A pipeline name |
| `on_findings` | What a review with blocking findings publishes: `request_changes` or `comment` |
| `on_clean` | What a review without blocking findings does: `approve`, `await` (wait for a human approval) or `comment` |
| `on_notes` | Optional. `comment`: a review whose findings are all notes goes to the author as a comment first, see [notes hand-off](knowledge/concepts/decisions.md#notes-hand-off). Pull requests authored by bots skip the hand-off and wait for approval directly |
| `authors` | Required. Who counts as a trusted author and what an untrusted author gets, see below |
| `human_review` | Optional, needs `on_clean: approve`. The signals that hold a clean review for a person, see [human review signals](#human-review-signals) |

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

### Human review signals

`human_review` holds a clean review of an approving rule for a person. A matching
signal turns the approval into a wait for a human approval, the same wait as
`on_clean: await`, and the review body and the notification name each matched
signal. A review with blocking findings keeps its `on_findings` outcome, and the
verifier escalation keeps its own wait.

Every key is optional and an absent key turns its signal off. The section needs
at least one key.

| Key | Matches when | Example |
|---|---|---|
| `labels` | the pull request carries one of the labels, case-insensitively; an author adds one to ask for a human | `labels: [needs-human]` |
| `deleted_tests` | the pull request deletes (status `removed`) a file that matches one of the globs | `deleted_tests: ["**/tests/**"]` |
| `paths` | a changed file matches one of the globs, such as migrations, permission code or CI files | `paths: ["migrations/**", "**/auth/**", ".github/**"]` |
| `max_changed_lines` | the additions and deletions of the pull request add up to more than the number | `max_changed_lines: 1000` |
| `max_changed_files` | the pull request changes more files than the number | `max_changed_files: 40` |
| `max_notes` | the finished review has more notes (non-blocking findings) than the number | `max_notes: 5` |
| `authors` | the author login matches an entry, case-insensitively, or the author class is listed | `authors: ["octocat"]` |

Rules:

- Globs are slash-separated relative patterns. `*`, `?` and character classes
  match within one path segment, and `**` as a whole segment matches any number
  of segments: `**/tests/**` matches `tests/a.php` and `pkg/tests/unit/a.php`, but
  not `src/testsuite/a.php`.
- Globs match ASCII letters case-insensitively, on the pattern and on the path:
  `db/**` matches `DB/X.SQL`.
- A renamed file matches `paths` on its new path and on its old path, so moving a
  migration out of `db/` still holds the review. A rename out of a `deleted_tests`
  glob counts as a deleted test (the old path is listed), unless the new path still
  matches one of the globs, so moving a test out of the test tree is never silent.
- Logins are lowercase. The names `trusted` and `untrusted` are read as author
  classes, so a login with either name cannot be listed. A class entry matches
  the author class that the policy decided for the review.
- Each list holds at most 50 entries. Every label, glob and author is listed
  once, and every number is a positive integer. A value that breaks one of these
  rules fails the policy at startup.
- The changed files are read once per review, and only when a `paths`,
  `deleted_tests`, `max_changed_lines` or `max_changed_files` key is set. Line and
  file counts cover the whole pull request. A listing that stops at the engine's
  `search_page_limit` or at GitHub's cap of 3,000 files is truncated: the clean
  review waits for a human, with the signal `changed files exceed the listing limit
  (N listed)`, whatever the other keys say. A rule that sets none of those four keys
  never reads the listing, so truncation never holds it. A review with blocking
  findings keeps its `on_findings` outcome.
- The signals are evaluated when the review finishes and stored with it, so a
  resumed review keeps its decision.
- A path, label or login in a signal is a code span on one line, and each bidi
  control or zero-width character in it is shown as `<U+XXXX>`, such as
  `<U+202E>`, so a name cannot look like another.

```yaml
human_review: {labels: [needs-human], deleted_tests: ["**/tests/**"], paths: ["migrations/**", ".github/**"], max_changed_lines: 1000, max_changed_files: 40, max_notes: 5, authors: ["octocat"]}
```

### Authors

Every rule states who it trusts. There is no implicit default: a rule without
`authors` is an error, so a public repository cannot be opened to strangers by
omission. See [untrusted authors](knowledge/safety/untrusted-authors.md).

`trusted: any` reviews every author, forks included, with the rule's outcomes, approval
too. Use it only for a repository without outside contributors.

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
| `trusted` | `any`, or a mapping with `logins` (lowercase logins: a user as `name` or, for an Enterprise Managed User, `handle_shortcode`, a GitHub App or bot as `name[bot]`, quoted in YAML, such as `"devin-ai-integration[bot]"`), `permission` (`write`, `maintain` or `admin`), or both. An author matching either is trusted |
| `untrusted` | Required unless `trusted: any`. `skip`, or a mapping with `pipeline`, `on_findings` and `on_clean` |

Rules for the untrusted section:

- Its pipeline must use `checkout: base`, so code from the pull request never
  becomes the reviewer's working tree.
- `on_clean` may be `comment` or `await`, never `approve`; `on_notes` is not
  allowed.
- A pull request from a fork is untrusted unless its author is listed in
  `logins`; repository permission alone does not make a fork trusted.
- A login matches by author type, as GitHub reports it for the pull request. A
  plain `name` matches only a user, and `name[bot]` matches only an app or bot
  named `name`, so a user account that takes an app's name is not trusted by it.
  An author whose type is missing or neither is untrusted, and so is an app that
  is not listed: permission trusts only users.
- The author class is resolved when a review is dispatched and stored with it.
  The permission is read with the credential that serves the repository, only for
  an unlisted author of a pull request that is not from a fork, once per author and
  repository in a discovery pass. When the read fails, that author's pull requests
  are not started: a `review_stopped` notice names the error code and the read to
  check, and the next pass reads the permission again. A failed read never counts
  as trusted.

## Drafts of coding agents

Some coding agents open pull requests as drafts and never mark them ready.
GitHub does not count a draft as asking for review, so it would never be
reviewed. A draft by a login in `ready_draft_authors` that requests a review
from the repository's credential login is marked ready for review, then reviewed like any other
pull request. GitHub search names an app author with the `app/` prefix;
write the plain login here.

## Notifications

Every event gets one of three levels. Where each level goes, and which single
events have their own route, is configured in [config.yaml](configuration.md#notify).

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
| `review_stopped` | A review failed after its retries, reviews wait for a review host, or an approved review could not be published |
| `cleanup_manual` | A reviewer session could not be removed and needs a manual `pohunek session rm` |
| `command_failed` | An operator command (approve, retry) failed |
| `approved` | An approval was published |
| `changes_requested` | A request for changes was published |
| `commented` | A comment review was published |
| `approval_expired` | A wait for approval passed `approval.timeout_hours` |
| `started` | A review started |
| `retry_scheduled` | A failed review step is retried later |
| `superseded` | A newer commit or a retarget to another base branch replaced a review, or the pull request stopped asking for one (review request or label removed, closed, back to draft) |

Events for a rejected or an expiring GitHub token are planned in #19.

A message the notifier cannot deliver is dropped with a warning in the log, at
every level; neither the review nor discovery waits for it or sends it again
(see [notifications](knowledge/concepts/notifications.md#delivery-failure)). A
review that waits for approval stays listed by `autoreview awaiting` without
its message.

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
| `authors is required` | State who the rule trusts; `trusted: any` reviews every author, forks included, with the rule's outcomes |
| `untrusted pipeline must use checkout base` | Use a base-checkout pipeline for untrusted authors |
| `authors.untrusted is required unless trusted is any` | Add `untrusted: skip` or an untrusted mapping |
| `authors.untrusted.on_clean must be comment or await, never approve` | Use `comment` or `await` |
