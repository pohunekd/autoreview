---
type: Guide
id: guide/coding-agent-drafts
title: Coding-agent drafts
description: Drafts opened by coding agents that request a review are marked ready for review and reviewed like any other pull request, with the policy, the ready-draft rule, the author-login caveat for app authors and the commands to check a draft before it is approved.
source_kind: manual
intents: [setup, help]
---

# Coding-agent drafts

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

Some coding agents open their pull requests as drafts and never mark them ready.
GitHub does not count a draft as a request for review, so an unmarked draft would
never be reviewed. autoreview marks a draft ready when its author is a listed
coding agent and it requests a review from the review account. The pull request
is then reviewed like any other, and clean reviews wait for a person.

Example policy: [examples/coding-agent-drafts/policy.yaml](../../../examples/coding-agent-drafts/policy.yaml).

## When to use it

- Coding agents such as Devin or Copilot's coding agent open pull requests against
  your repository.
- You want their work reviewed by the same reviewers as human work, and nothing
  merged on a clean review without you.

Do not use it when:

- Pull requests from agents are not wanted at all. Remove the agents from
  `ready_draft_authors` and the trusted logins, and the rule reviews them as any
  other untrusted author.
- The agent does not request a review from the review account. The draft is
  then not a candidate and is never marked ready.

## The policy

```yaml
reviewers:
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
    reviewers: [sonnet]
    incremental_reviewers: [sonnet]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]
  light:
    reviewers: [codex]
    incremental_reviewers: [codex]
    prompt: prompts/generic.md
    checkout: base
    project_sources: [manual]

repositories:
  - match: zajca/pohunek
    trigger: review-requested        # drafts are marked ready only for a review request
    pipeline: core
    on_findings: request_changes
    on_clean: await                  # a person approves agent work too
    authors:
      trusted:
        logins: [devin-ai-integration, copilot-swe-agent]
        permission: write
      untrusted:
        pipeline: light
        on_findings: comment
        on_clean: comment

unmatched: skip

ready_draft_authors: [devin-ai-integration, copilot-swe-agent]

approval:
  timeout_hours: 168
```

Key lines:

- `ready_draft_authors` lists plain lowercase logins. GitHub search names an app
  author with the `app/` prefix, but the policy takes the plain login. The list
  must be lowercase and each login listed once.
- A draft is marked ready only if it requests a review from the `login` of the
  credential that serves the repository. The trigger is therefore `review-requested`,
  not a label.
- `on_clean: await` keeps the agent's work behind a person's approval.
- The untrusted section gives anyone else a comment from the base checkout.

## Caveat: how the author login is matched

The reference says `authors.trusted.logins` takes lowercase logins, and that
`ready_draft_authors` takes plain logins. It does not say how a pull request
opened by an app is matched against those lists. Before you rely on the trusted
side for an agent, run `autoreview candidates` and `autoreview review` on one of
its pull requests and check which author class it was given. If it lands in the
untrusted section, the agent's pull requests get a comment only.

## Walk-through of one agent draft

Devin opens PR #64 in `zajca/pohunek` as a draft and requests a review from the
review account.

1. **Discovery.** The review request on a draft makes it a candidate. The author's
   login is in `ready_draft_authors`, so autoreview marks PR #64 ready for review
   before it starts the review.
2. **On GitHub.** The draft becomes a normal pull request marked ready. Human
   reviewers see it the same way as any other.
3. **Review.** Sonnet reviews the change from the base branch, read-only.
4. **Blocking findings.** The review is published as REQUEST_CHANGES. The author
   sees the inline comments and the signature line
   (`Written by AI: autoreview (posted from review-bot's GitHub account, not by a human)`).
5. **Clean review.** `on_clean: await` leaves the pull request waiting. The
   `approval_requested` event at level `action` is raised. Approve the head
   you checked:

```sh
autoreview --config /etc/autoreview/config.yaml awaiting
autoreview review --repository zajca/pohunek 64
autoreview resolve --repository zajca/pohunek 64 3f2a9c1d0b7e
autoreview approve --repository zajca/pohunek 64 <full-head-sha>
autoreview approve --repository zajca/pohunek 64 <full-head-sha>   # shows published
```

6. **Agent pushes again.** A new commit supersedes the wait (`superseded`, level
   `log`) and the new head is reviewed. The agent's notes-only review does not
   produce a hand-off: bot authors skip the notes hand-off. Its clean review
   waits for approval directly.

A draft that is not requested for review is ignored, even when its author is
listed. Nothing is marked ready, and nothing is reviewed until a request arrives.

## Commands

```sh
autoreview --config /etc/autoreview/config.yaml candidates   # drafts that would be marked ready
autoreview review --repository zajca/pohunek 64              # state and author class of PR #64
autoreview retry --repository zajca/pohunek 64 3f2a9c1d0b7e  # after review_stopped
```

## Variations

- **Another agent.** Add its plain login to `ready_draft_authors`, and to
  `authors.trusted.logins` if its pull requests should be trusted. Keep both lists
  lowercase and unique.
- **Agent work is commented, not approved.** Change `on_clean: await` to
  `on_clean: comment`. Clean agent work then gets a comment and no approval wait,
  which suits a repository where the agent's pull requests are merged by a
  maintainer's review anyway.
- **Agent work is approved automatically.** Change `on_clean` to `approve`, add
  the same reviewer list to `incremental_reviewers` (already equal here), and read
  [auto approve](auto-approve.md) first. This removes the person from the agent's
  path, which is rarely the right choice for code written without supervision.
- **Stricter agents.** Set `trusted: {logins: [...]}` without `permission`, so only
  the listed agents are trusted and every other author is untrusted.

## Combine with

- [Human approval](human-approval.md) for the approval flow that agent work ends in.
- [Notes hand-off](notes-handoff.md) for human authors, since bot authors skip it.
- [Public repositories](public-repositories.md) for the untrusted side and for
  repositories where agents are not the only outside authors.
