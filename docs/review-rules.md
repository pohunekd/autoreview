---
type: Reference
id: reference/review-rules
title: Repository review rules (REVIEW.md)
description: The REVIEW.md file that a repository keeps at its root to tell autoreview which paths never get findings, which tools its CI already enforces and what its maintainers want reviewers to know - its format, where the engine reads it, what the reviewers and the verifier receive, how a skipped finding is published, and every error code.
source_kind: manual
intents: [configure, help]
---

# Repository review rules (REVIEW.md)

> **Status: after 0.6.0.** A pipeline with `repository_review_rules: read` reads
> the `REVIEW.md` of the repository's base commit; with `ignore` the file is
> never read.

The team that owns a repository can keep review rules in a `REVIEW.md` at the
repository root. The engine reads the file from the pull request's **base
commit** (the `baseRefOid` that GitHub reports and the request already names as
the trusted base SHA), never from the pull request head, so a pull request
cannot change the rules that review it. A fork's pull request shares the base
repository's content and cannot influence them either.

## Example

```markdown
---
skip:
  - "docs/archive/**"
  - "**/CHANGELOG.md"
ci_tools:
  - rustfmt
  - clippy
---
Public API changes need a note in the changelog. Do not ask for comments on
private helpers.
```

## Format

The file is UTF-8 Markdown. A file that starts with a `---` line has YAML front
matter that ends at the next `---` line; everything after it is the prose.
A file without that first line is all prose. A byte order mark and CRLF line
ends are accepted. The file is at most `limits.max_review_rules_bytes` bytes
([configuration](configuration.md#limits)).

| Front matter key | Value |
|---|---|
| `skip` | Optional list of at most 100 path globs relative to the repository root. `*`, `?` and `[...]` match inside one path segment and `**` matches whole segments, so `CHANGELOG.md` matches only a root-level file: write `**/CHANGELOG.md`. Each glob is 1 to 256 bytes, has no leading `/`, no `..` segment, no backslash, at most 4 `**` segments, and is listed once. `skip: null` is an error |
| `ci_tools` | Optional list of at most 32 tool names that CI already enforces, each 1 to 64 characters of letters, digits, space, `.`, `_`, `+`, `-` and `/`, listed once |

An unknown key, a wrong type, a front matter that is not closed, a glob or tool
name that breaks the rules above or a NUL character fails the review with
`review_rules_invalid`.

## What the reviewers and the verifier get

With `repository_review_rules: read`, every reviewer request and the verifier
request gets a section after the other sections: fixed engine instructions, then
the rules (`skip`, `ci_tools` and the prose as `text`) on one line of JSON. The
rules are untrusted data in the sense of
[prompt injection](knowledge/safety/prompt-injection.md): they guide what to
report, never what the prompt, the output format or the decision say. The
reviewers are told not to spend effort on skipped paths and not to report what
a `ci_tools` tool would report, and the verifier is told that a claim that only
restates such a tool is not blocking. These instructions come from the engine,
so they apply to every installed copy of the prompts.

The read happens once per review, as the step `review_rules`, and the rules are
stored with the review: a resumed or retried review sees the same text. A
review whose pipeline says `ignore`, a repository without a `REVIEW.md`, and an
adopted notes hand-off (no reviewer runs) read nothing and add nothing to the
requests.

The adapters' `--maximum-input-bytes` must cover the rules: JSON escaping can
make one byte of `text` up to 7 bytes of a request. Keep it at least
7 times `limits.max_review_rules_bytes` plus the prompt and the other
sections. The engine does not know that limit and never truncates the rules.
An adapter that gets a request over its limit exits with `initial input exceeds
the configured byte limit` before it prints a result, so the engine sees no
valid result: `review_output_incomplete` for an empty output or
`review_wrapper_invalid` (with the end of the adapter's stderr) for any other.
Both end the reviewer's run, which is retried with a fresh session up to
`session.reviewer_run_max_attempts` times with the same request, and then the
review fails with that code. For the verifier the claims are published
unverified. No code names `REVIEW.md`: when the stderr says `exceeds the
configured byte limit`, raise `--maximum-input-bytes` or lower
`limits.max_review_rules_bytes`.

## Skipped paths

The engine, not only the prompt, enforces `skip`. A blocking finding whose
reported path matches a glob becomes a note: it is published without asking for
changes, it never reaches the verifier, and its entry in the published review
reads `Skipped by REVIEW.md: the path matches `skip` pattern `<glob>`.` The
summary says how many findings became notes:

```text
Repository review rules: REVIEW.md at base commit `abc123456789` was applied; 2 finding(s) in skipped paths became notes.
```

`skip` lets anyone who can write to the base branch turn findings into notes,
so a review can be clean (and, on a rule that approves clean reviews, an
approval) because of what a REVIEW.md says. `repository_review_rules: read` is
therefore the operator's opt-in for that trust, per pipeline; `ignore` keeps
every finding. The rules never change a rule's decision: that stays in the
policy.

The author of a pull request chooses what is in a skipped path: a pull request
can put a change under a `skip` glob (for example into `docs/archive/`) and the
findings about it become notes. Do not skip lockfiles, generated or compiled
code, or any path whose content is executed or shipped. On a rule that approves
clean reviews, add the same globs to `human_review.paths` so that a change
there waits for a person ([policy](policy.md#human-review-signals)).

A finding about a skipped file that a reviewer reports on another path is not
matched, because the match uses the reported path. A finding of an earlier
review whose thread is still open keeps blocking until the thread is resolved,
also when its path is skipped now; once it is resolved and the finding comes
back, it is a note.

## A pull request that changes REVIEW.md

A pull request is reviewed with the base version of the file, and the summary
says so:

- The base has a `REVIEW.md`: "This pull request changes `REVIEW.md`. This
  review applied the rules at base commit `abc123456789`, not the version in
  this pull request."
- The base has none: "This pull request changes `REVIEW.md`. The base commit
  `abc123456789` has no REVIEW.md, so this review applied no repository review
  rules; the version in this pull request is not applied until it is merged."

The change is detected from the pull request's changed files (the path or the
source of a rename). A listing that is cut short (more than 3000 changed files) is
held as well, with the signal `changed files exceed the listing limit`, because
`REVIEW.md` could lie past its end.

On a rule that approves clean reviews, such a pull request is never approved
without a person: it counts as a human review signal (`changes REVIEW.md`), also
when the rule has no `human_review` section, so the review waits for the user as
for a match of `human_review.paths`. Otherwise an author whose clean pull
requests are approved could add `skip: ["**"]` and have the engine approve it.
A rule that does not approve clean reviews is not affected. The hold applies only
while the pipeline reads `REVIEW.md`: check the base `REVIEW.md` of each
repository before switching a pipeline from `ignore` to `read`.

## Errors

| Code | Cause | Effect |
|---|---|---|
| `review_rules_too_large` | The base file is over `limits.max_review_rules_bytes` | The review fails |
| `review_rules_invalid` | Binary, not UTF-8 (most lossy decodes are detected, not all), truncated, or a front matter error | The review fails |
| `review_rules_unreadable` | The read failed for a permanent cause, such as a base commit the repository does not have | The review fails and is not retried automatically |
| `github_request_failed` | The read failed for a transient cause | Retried inside the review step (`schedule.step_retry_seconds`), then the review fails with this code and the scheduler retries it like any other GitHub read |

A broken file is never ignored silently. The cause names the key or the line and
goes to the service log (the warning `REVIEW.md of the base commit is unusable`
with the review ID and the base commit), never to the pull request. The failed
review is announced like any failed review: the scheduler's `review_stopped`
notice names the code (`review_rules_invalid`, `review_rules_too_large`) and
points to the service log.

The exception is a pull request that changes `REVIEW.md`: when the base file is
too large or invalid, that pull request, which may be the fix, is reviewed
without rules, the summary says the base `REVIEW.md` is invalid, names the code
and says that no rules were applied, and one `review_stopped` notice names the
code, although the review goes on (the policy has no event for a degraded review,
so route `review_stopped` accordingly). A failed read always fails the review,
also for a pull request that changes `REVIEW.md`.

## Rollback

Older releases refuse `repository_review_rules` in the policy and
`limits.max_review_rules_bytes` in `config.yaml`; remove both to roll back, see
[roll back](knowledge/runbooks/upgrade-and-backup.md#roll-back). A review
stored by this release and read by an older one runs without rules.
