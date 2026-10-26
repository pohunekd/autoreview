---
type: Concept
id: concept/verification
title: Verification
description: How the optional verifier re-checks blocking findings and answered prior findings, the verdicts it can return, how unconfirmed findings are demoted to notes with a reason, how answered findings are reopened, what happens when the verifier fails, and how escalation turns an auto-approval into a wait for a human.
source_kind: manual
intents: [setup, help, debug]
---

# Verification

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

A reviewer can be wrong. The verifier is a second, separate agent run that checks
the claims of the reviewers before anything is published. It is optional and it
is set per pipeline with the `verifier` key (see
[policy: verifier](../../policy.md#verifier)).

```yaml
pipelines:
  core:
    reviewers: [opus, sonnet]
    incremental_reviewers: [opus]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]
    verifier:
      reviewer: opus                 # must use protocol claude-structured
      prompt: prompts/verifier.md
      escalate_severities: [critical]
      escalate_count: 3
```

## What is verified

The engine sends the verifier two kinds of claims. Nothing else is checked.

| Claim | Source | Verdicts |
|---|---|---|
| A blocking finding | Any reviewer of this review, after anchoring | `confirmed`, `not_blocking`, `refuted` |
| An answered prior finding | A reviewer's `answered` verdict on an earlier blocking finding | `upheld`, `reopened` |

Findings that are notes are not claims. A finding in a file the pull request does
not change is already a note at that point, so it never reaches the verifier (see
[how it works](how-it-works.md#6-anchoring-findings-to-changed-lines)).

The verifier runs after the reviewers, in its own session, with its own deadline.
It is not one of the reviewers of the review, and its session is separate from
theirs. If a review has no claims, the verifier does not run.

## What the verifier receives

The request has two parts:

- **Trusted.** The pipeline's verifier prompt, the repository, the pull request
  number, the head SHA, the base branch and the fixed instructions of the
  verifier.
- **Untrusted.** The claims as JSON: the finding id, the reviewer, the severity,
  the path, the line, the title and the body of each finding, and for prior
  claims the earlier verdict and the login it relies on. When there are prior
  claims, the discussion of the pull request follows, also as untrusted JSON.

Everything the pull request says is data. The verifier prompt tells the verifier
to check each claim against the code at the head.

## Verdicts

Each claim gets exactly one verdict. The output is checked against the claims:
every claim must appear once, and nothing may be invented, skipped or repeated.
Output that does not match gets a fresh run.

| Claim | Verdict | Effect |
|---|---|---|
| Blocking finding | `confirmed` | Stays blocking |
| Blocking finding | `not_blocking` | Becomes a note. The body says it is not blocking according to the verification, with the verifier's reason. |
| Blocking finding | `refuted` | Becomes a note. The body says it was refuted by the verification, with the reason. |
| Answered prior finding | `upheld` | The reviewer's answer stands, and the prior finding is closed |
| Answered prior finding | `reopened` | The prior finding is open again, with the verifier's reason |

Nothing is dropped. A demoted finding is still published, as a note with its
reason, so the author can see what the verifier decided and why. Notes are
listed separately, and the notification of the review names the verdict in the
note line, for example `NOTE, REFUTED BY VERIFICATION`.

The verification line of the review body reports the counts. For example:

```text
Verification by Opus: blocking findings 1 confirmed, 0 not blocking, 0 refuted; earlier findings settled in the discussion 1 upheld, 0 reopened.
```

## Reopening answered prior findings

A reviewer may call an earlier finding answered. A `reopened` verdict from the
verifier turns that answer back into an open finding. The reason of the verifier
is added to the note of the finding, and the note also records the reviewer's
earlier answer. The finding then counts as blocking again.

Reopening only applies to answers. A prior finding the reviewers left open is not
a claim and is not sent to the verifier.

## When the verifier fails

If the verifier does not finish, the claims stand as the reviewers made them. The
review is still published. Its verification record carries the error code, and
the body says that the verification did not finish and that the findings are
published as the reviewers reported them. The log records the review and the
error code.

Only an error of the review itself stops it, for example a head that changed while
the verifier ran. The same rule applies to the verifier's reviewer session as to
any other reviewer. A session that gives unusable output gets a fresh run, up to
`session.reviewer_run_max_attempts`.

## Escalation on auto-approve rules

A rule with `on_clean: approve` publishes an approval when nothing blocks. The
verifier can demote blocking findings, which could make an approval look safe
when a reviewer had found a serious defect. Escalation sends such a review to a
human instead. It applies only to rules with `on_clean: approve`.

A review is escalated when the verifier demoted:

- at least one blocking finding whose severity is in `escalate_severities`, or
- at least `escalate_count` blocking findings, of any severity.

An escalated review that would have been clean is not approved. It waits for
approval as a `await` does, the `approval_requested` event is sent, and the
`approve` command applies to it. Escalation never happens on a rule that does not
approve, because those rules already wait for a human or request changes.

### Worked example

Policy: a rule with `on_clean: approve` on pipeline `core`, with
`escalate_severities: [critical]` and `escalate_count: 3`. Two reviewers ran, and
the verifier returned verdicts for every claim.

| Case | Blocking findings from reviewers | Verifier verdicts | Demoted | Escalated | Outcome |
|---|---|---|---|---|---|
| A | `O001` critical | `O001` refuted | 1, severity `critical` is in `escalate_severities` | Yes | Wait for a human, then `APPROVE` |
| B | `O001` high, `S001` high | both refuted | 2, neither severity escalates, fewer than 3 | No | `APPROVE` |
| C | `O001` high, `S001` high, `O002` high | all refuted | 3, which meets `escalate_count` | Yes | Wait for a human, then `APPROVE` |
| D | `O001` high | `O001` confirmed | 0 | No | `REQUEST_CHANGES` (on `on_findings: request_changes`) |

In case A the verification line reads `blocking findings 0 confirmed, 0 not
blocking, 1 refuted`, and the review is shown as waiting. Anyone who runs
`autoreview approve` with the head SHA publishes the approval. Without that step
nothing is posted, and the wait ends with `approval_expired` after
`approval.timeout_hours`.

The example uses the keys of the reference. Escalation only changes the outcome
of a clean review. A review with confirmed blocking findings keeps the outcome of
`on_findings`.

## Settings that matter

| Key | Effect |
|---|---|
| `verifier.reviewer` | The reviewer that runs the verifier. Must use `claude-structured`. |
| `verifier.prompt` | Path of the verifier prompt, relative to the policy file. |
| `escalate_severities` | `critical`, `high`, or both. Demoting a finding of these severities escalates. |
| `escalate_count` | At least 1. Demoting this many blocking findings escalates. |
| `limits.max_body_characters` | Bound on each verifier note. |

## Related pages

- How the verifier fits into one review: [how it works](how-it-works.md#7-verification).
- Prior findings and their reopening: [incremental reviews](incremental-reviews.md#prior-findings).
- The waits and the decision after escalation: [decisions](decisions.md#approval-wait).
- Writing the verifier prompt: [writing prompts](../guides/writing-prompts.md).
- Human approval as a setup: [human approval](../guides/human-approval.md).
