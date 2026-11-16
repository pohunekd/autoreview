---
type: Reference
id: reference/policy
title: Policy reference
description: Every key of policy.yaml - reviewers, pipelines, repository rules, author classes, approval, notes hand-off, draft handling and notification levels - with the rules autoreview validates at startup.
source_kind: manual
intents: [setup, project, help]
---

# Policy reference

> **Status: after 0.4.0.** This page describes autoreview on `main` after the
> 0.4.0 release. A part that is not available names its open issue.

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
per owner, each with its own token, works too.

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
| `notifications` | yes | A level for every [event](#notifications); `credential_invalid`, `credential_restored` and `credential_expiring` may be left out |

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
| `checkout` | `base`: the worktree starts at the base branch tip, so repository guidance is read from trusted content; the reviewer reads the change through git (`git diff <base>...<head>`, `git show <head>:<path>`) without checking the head out. With pohunek 0.43.0 or later (the CLI and the review host's daemon) the session start fetches `refs/pull/<n>/head` at the trusted head without checking it out; with an older pohunek only the base branch is fetched, so the head commit must already be in the repository. Without the head the Claude and Codex adapters stop the review with `review_head_unavailable` and nothing is published. `head`: the worktree starts at `refs/pull/<n>/head`, so pohunek runs the hooks of the pull request head (`.pohunek/`), and with them any code they run (see the [threat model](knowledge/safety/threat-model.md#residual-risks)) |
| `project_sources` | Which pohunek projects may serve the repository: `manual` (registered with `pohunek project add`), `auto` (discovered), or both. autoreview checks only the form of a name (lowercase letters, digits, `_` and `-`, at most 32 characters) and passes it to pohunek unchanged |
| `verifier` | Optional, see below |
| `intent_check` | Optional. `issues`: the reviewers compare the change with the issues the pull request closes, see [intent check](#intent-check). `off`, or no key: no comparison. `intent_check: null` is an error |

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

### Intent check

With `intent_check: issues`, the engine reads the GitHub issues that the pull
request closes (its closing references, up to ten) once per review, with the
credential of the review. Only issues of the pull request's own repository are
read: an issue of another repository may be private while the review is
public, so its text is never requested, and the published review says only
that issues of other repositories were not compared, without naming them.
Their titles and bodies share `limits.max_issue_characters` characters
([configuration](configuration.md#limits)); a text over the limit is cut, ends
with `…` and is marked `truncated`, and an issue past the limit keeps its
reference with an empty text. The issues reach the reviewers, and the verifier,
as untrusted data on one line of JSON after the trusted request context.

The engine adds the intent pass to the request: a clear mismatch (the change
does something other than an issue asks, or leaves out a part the issue makes
essential) is a blocking finding; every other difference goes to the summary.
A pull request that closes no issue is not a finding. The published review
states which issues were compared, that none is linked, or that they could not
be read: a failed or refused read never fails the review, it is logged as
`linked issues could not be read`. Without the key, or with `off`, nothing is
read and the request is unchanged. The key changes the policy digest only when
it is `issues`.

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
| `author_limit` | Optional. How many reviews one author may start under the rule in a window, see [author limit](#author-limit). Without it the rule sets no limit |
| `guard` | Optional. The prompt-injection guard of the rule, see [guard](#guard). Without it the rule runs no guard |
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

### Author limit

`author_limit` bounds the reviews started for the pull requests of one author
under the rule. Without the key the rule has no limit. With it, every field is
required, and `author_limit: null` is an error.

```yaml
author_limit:
  reviews: 10        # 1-1000 reviews of one author in the window
  window_hours: 24   # 1-720
  over: wait         # wait or skip
```

- The count is per author login, matched without regard to case, across the
  repositories of the rule. It covers every author the rule reviews, trusted or
  untrusted.
- Every started review counts: a first review, an automatic retry, a review
  started again after a new commit, a moved base branch or a removed and
  re-added trigger, and a `retry`. A review counts from its start for
  `window_hours`, and the count is read from the store, so a restart keeps it.
- A new start over the bound is held. An automatic retry of a failed review and
  a `retry` command are never held, also when they wait for a review host first,
  so a review that started can finish.
- `over: wait`: the review waits and starts on the first discovery pass after
  enough starts left the window. `over: skip`: the review of that head is not
  started, and a `review_stopped` notice with the code `author_limit_reached`
  names it. A later commit of a skipped pull request is planned again and, when
  it is skipped too, sends no second notice, also after it waited for a review
  host. The pull request is notified again after one of its reviews started or
  failed, and after it stops asking for a review and asks again. `retry`
  starts the skipped head at once.
- Each held review logs `review held by the author limit` with the author, the
  rule, the outcome and, for a wait, when it is due.
- `window_hours` must not be longer than `store.review_retention_seconds`,
  because removed reviews leave the count; `validate` refuses a longer window.

The limit counts reviews per pull request author. It does not know who added
the label or requested the review.

### Guard

`guard` opts the rule in to the prompt-injection guard, a deterministic scan of
the pull request text for instructions hidden from a human reader. It runs once
per review, before the reviewers start, and stores its result with the review,
so a resumed review keeps it. Without the key the rule has no guard and the
review is unchanged. With it, every field is required, and `guard: null` is an
error.

```yaml
guard:
  fields: [title, body, commit_message, comment, review_thread, patch]
  threshold: 100       # 1-4294967295
  on_suspect: await    # await, comment or log
```

- `fields`: the pull request texts the guard scans, at least one, each once:
  `title`, `body`, `commit_message`, `comment`, `review_thread` and `patch`
  (the added lines of the changed files). Their order does not matter.
- `threshold`: the guard score from which a pull request counts as suspect.
  Scores start at 0, so 0 is refused: it would mark every pull request.
- `on_suspect`: the outcome for a suspect pull request:
  - `await`: a clean review that the rule would approve waits for a human
    approval, as with [human review signals](#human-review-signals); nothing is
    posted before the approval. Other reviews are published as usual.
  - `comment`: a request for changes on an untrusted author's pull request is
    published as a comment.
  - `log`: nothing changes; only the stored result and the notification remain.
- A pull request is suspect when its score reaches `threshold`, when the
  configuration has no `guards.text` section, or when the read left text out,
  so the rest was not scanned:
  - the text reached `guards.text.max_input_bytes`, or a list reached the page
    limit (`schedule.search_page_limit` pages of 100);
  - a review thread has more than 100 comments, or a comment GitHub does not
    return;
  - a changed file has added lines but no patch (GitHub leaves out the patch
    of a large diff);
  - the changed-files listing reached 3000 files, GitHub's limit, or has fewer
    files than the pull request.

  The missing section and a stored guard this engine cannot read behave as
  `await`.
- No outcome requests changes: the guard never blocks a pull request.
- Every suspect pull request sends one `guard_suspect` message at level
  `action`, whatever the outcome. It names the signal codes, the score and the
  outcome, never the matched text. A retry of the same review that finds the
  same signals sends none. With `await` or `comment`, the published review
  names the signal codes in one line.
- The guard reads its fields with extra GitHub calls, only for a rule with
  `guard`: a GraphQL query for the title, the body and the changed-file count
  (also for `patch`), one paged query per list (commits, comments and reviews,
  review threads) and the changed-files listing for `patch`. Comments, reviews
  and review-thread comments of the reviewing account are not scanned.
- A rule with `guard` needs the [`guards.text`](configuration.md#guards)
  section of the configuration; without it the configuration fails to load.
- A rule with `guard` changes the policy digest; a policy without one keeps its
  digest.

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
| `credential_invalid` | GitHub refuses a credential's token while the service runs, which suspends the credential; once per suspension |
| `credential_restored` | GitHub accepts the token of a suspended credential again, which lifts the suspension; once per lift |
| `credential_expiring` | A credential's token expires within `github.expiry_warning_days`; at most once a UTC day per credential while `serve` runs |
| `started` | A review started |
| `retry_scheduled` | A failed review step is retried later |
| `superseded` | A newer commit or a retarget to another base branch replaced a review, or the pull request stopped asking for one (review request or label removed, closed, back to draft) |

`credential_invalid`, `credential_restored` and `credential_expiring` are
optional: a policy without them sends them at level `action`, and its digest
stays the same. A level set for one is used and changes the policy digest.

`guard_suspect` (a rule's [guard](#guard) found a pull request suspect) is
always sent at level `action`; a policy cannot name it.

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
| `pipeline <name>: intent_check must be off or issues` | Set `off` or `issues`, or remove the key |
| `authors.untrusted is required unless trusted is any` | Add `untrusted: skip` or an untrusted mapping |
| `authors.untrusted.on_clean must be comment or await, never approve` | Use `comment` or `await` |
| `author_limit.reviews must be between 1 and 1000` | Set `reviews`; also shown when it is missing |
| `author_limit.window_hours must be between 1 and 720` | Set `window_hours`; also shown when it is missing |
| `author_limit.over must be wait or skip` | Set `over` |
| `author_limit must be a mapping with reviews, window_hours and over` | Fill in the key, or remove it for no limit |
| `guard must be a mapping with fields, threshold and on_suspect` | Fill in the key, or remove it for no guard |
| `guard.fields must name at least one field` | List the texts to scan; also shown when `fields` is missing |
| `guard.fields: "x" is not one of title, body, commit_message, comment, review_thread, patch` | Use those names, in lowercase |
| `guard.fields: "x" is listed twice` | List each field once |
| `guard.threshold must be between 1 and 4294967295` | Set `threshold`; also shown when it is missing |
| `guard.on_suspect must be await, comment or log` | Set `on_suspect` |
