---
type: Guide
id: guide/auto-approve
title: Automatic approval
description: One Sonnet reviewer with a verifier on a low-risk website repository, where a clean review is approved without a person and a demoted serious finding turns the approval into a wait - with the policy, the rules that make automatic approval safe and the commands to inspect a wait.
source_kind: manual
intents: [setup, help]
---

# Automatic approval

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

A clean review is approved by the review account without a person. This is
acceptable only where a wrong approval is cheap: a website whose changes are
reviewed by the maintainers anyway, and where authors are people with write
permission. The verifier and its escalation are what make this safer than a
plain auto-approval.

Example policy: [examples/auto-approve/policy.yaml](../../../examples/auto-approve/policy.yaml).

## When to use it

- The repository is low risk: content, a documentation site, a static page.
- Every author who can trigger a review has write permission, so an unknown
  author cannot obtain an approval.
- You accept that the approval is the review account's, and that branch
  protection or a human merge is still your final gate.

Do not use it when:

- A wrong approval would reach production. Use [human approval](human-approval.md).
- Strangers open pull requests. This example skips them. Give them a
  comment-only rule as in [public repositories](public-repositories.md).

## The policy

```yaml
reviewers:
  sonnet:
    protocol: claude-structured
    agent_profile: autoreview-sonnet
    label: Sonnet
    finding_prefix: S

pipelines:
  site:
    reviewers: [sonnet]
    incremental_reviewers: [sonnet]  # must equal reviewers for on_clean approve
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]
    verifier:
      reviewer: sonnet              # claude-structured, as the verifier requires
      prompt: prompts/verifier.md
      escalate_severities: [critical, high]
      escalate_count: 2

repositories:
  - match: pohunekd/pohunek.dev
    trigger: label:ai:review       # a maintainer adds the label; it needs write permission
    pipeline: site
    on_findings: request_changes
    on_clean: approve              # approved without a human, if the verifier did not escalate
    authors:
      trusted:
        permission: write
      untrusted: skip              # not reviewed, so never approved

unmatched: skip

approval:
  timeout_hours: 168               # the wait that an escalated approval turns into
```

Key lines:

- `on_clean: approve` is valid only because `incremental_reviewers` lists the
  same reviewer as `reviewers`. Nobody confirms an automatic approval, so a
  re-review must not rest on fewer reviewers than the full review. The validator
  rejects the rule with `on_clean approve needs incremental_reviewers equal to reviewers`
  if the lists differ.
- `verifier` re-checks blocking findings. On a rule that approves, demoting a
  finding of an `escalate_severities` severity, or `escalate_count` or more
  findings, turns the approval into an `await`. The approval then waits for a
  person, and the `approval_requested` event at level `action` is raised.
- `escalate_count: 2` means two demoted findings are enough to stop the automatic
  approval, even when neither is critical or high.
- `untrusted: skip` is what keeps the approval from ever reaching a stranger's
  pull request. Skipped pull requests are not reviewed at all.

## Walk-through of one pull request

A maintainer opens PR #17 in `pohunekd/pohunek.dev` and adds the label
`ai:review`.

1. **Trigger.** The label makes PR #17 a candidate on the next discovery pass.
   The debounce waits for the pushes to settle before the review starts.
2. **Review.** Sonnet reviews the change in its own pohunek session, read-only,
   from the base branch.
3. **Verification.** The verifier (also Sonnet) re-checks the blocking findings.
4. **Clean, no escalation.** The review is published as an APPROVE from the
   review account. The `approved` event (digest) is recorded. The author sees the
   approval with the summary and the signature line.
5. **Blocking findings remain.** The review is published as REQUEST_CHANGES.
   `changes_requested` (digest) is recorded. A new push is reviewed again. The
   rule approves on its own, so it gets a full review after a request for changes.
6. **Escalation.** Suppose the verifier demotes one critical finding. The
   approval does not go out. The review waits, `approval_requested` is raised at
   level `action`, and a person decides with the commands below. An escalated
   approval waits `approval.timeout_hours` (168 hours in this example) before it
   expires with `approval_expired`.
7. **A new commit.** A push to PR #17 supersedes the earlier review (`superseded`,
   level `log`). The new head is reviewed by the same rule.

Notifications use the defaults that `init` writes, which send every level to
the structured log. Add a backend to see `approval_requested` or
`findings_on_own_pr` anywhere else; see the [notifiers guide](notifiers.md).

## Inspect and approve an escalated wait

```sh
autoreview --config /etc/autoreview/config.yaml awaiting
autoreview review --repository pohunekd/pohunek.dev 17
autoreview resolve --repository pohunekd/pohunek.dev 17 3f2a9c1d0b7e
autoreview approve --repository pohunekd/pohunek.dev 17 <full-head-sha>
autoreview approve --repository pohunekd/pohunek.dev 17 <full-head-sha>   # shows published
```

An approval that was never escalated is already published, so `approve` answers
`approval_not_required` for it. Use `autoreview review` to see which case you
are in.

If a review stopped after its retries (`review_stopped`), retry the head it
stopped on:

```sh
autoreview retry --repository pohunekd/pohunek.dev 17 3f2a9c1d0b7e
```

`autoreview review-queue` lists the open pull requests that wait on someone,
including those waiting on their author. Run it from a timer if you want that
list on a schedule.

## Variations

- **Stricter escalation.** Set `escalate_severities: [critical, high]` and
  `escalate_count: 1` to hand every serious demotion to a person.
- **Two reviewers.** Add a second Claude reviewer to `reviewers` and to
  `incremental_reviewers`, in the same order. Both lists must stay equal.
- **Human approval for everything.** Change `on_clean: approve` to
  `on_clean: await`. This is [human approval](human-approval.md) with one reviewer.
- **Label to review request.** Change the trigger to `review-requested` so that
  the review account starts reviews, not a label.
- **Comment for strangers.** Replace `untrusted: skip` with an untrusted pipeline
  and `on_findings: comment`, `on_clean: comment`. An untrusted author never gets
  an approval, because `approve` is not allowed on the untrusted side.

## Combine with

- [Human approval](human-approval.md) for the same verifier with a person in
  front of every clean review.
- [Public repositories](public-repositories.md) for the untrusted side of a
  repository that takes pull requests from strangers.
- [Organisation rules](org-wildcard.md) where this repository is a wildcard match.
- [Writing prompts](writing-prompts.md) for the reviewer and verifier prompts,
  which decide what counts as blocking.
