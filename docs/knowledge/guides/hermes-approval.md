---
type: Guide
id: guide/hermes-approval
title: Approve from chat with Hermes
description: Planned optional integration (#43) that lets a reply to an autoreview approval notification in a Hermes Agent chat approve the clean review - the planned skill and gateway guard plugin, the flow through resolve and approve, and the security invariants that keep the chat from publishing anything else.
source_kind: manual
intents: [setup, help]
---

# Approve from chat with Hermes

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0.

A rule with `on_clean: await` waits for a person before its clean review is
approved on GitHub. Today the person approves from the CLI with
`autoreview resolve` and `autoreview approve`. Approving from a chat with
[Hermes Agent](https://github.com/NousResearch/hermes-agent), through a skill
and a gateway guard plugin, is planned in #43; the repository contains neither
yet, and the rest of this page describes their design. Nothing else in
autoreview depends on Hermes.

The planned integration does one thing: turn a reply `approve` to an approval
notification into the two commands the CLI already has. It is not to review,
retry, poll or post anything.

## The planned flow

1. autoreview finishes a clean review under a rule with `on_clean: await`. The
   review waits, and the event `approval_requested` is sent (its level is
   `action` in the example policies).
   The message contains the review identity in the form
   `<owner>/<repo>#PR<number>@<head-prefix>`, the first 12 characters of the
   head SHA, and the list of non-blocking notes.
2. The person replies to that notification in the chat with `approve`. On
   Matrix the reply is a real reply to the bot's message.
3. The gateway guard plugin looks up the replied-to notification. If the bot
   posted it and it carries a Review ID, the plugin quotes the notification
   text in front of the reply, so the agent knows which review is meant.
4. The skill extracts the repository, the pull request number and the
   12-character prefix from the quoted Review ID, and runs
   `autoreview resolve --repository <owner/repo> <number> <prefix>`. The
   command is read-only and prints the full `head_sha` of the waiting review.
5. The skill runs `autoreview approve --repository <owner/repo> <number> <head_sha>`
   with the full SHA from step 4.
6. The skill returns the exact JSON result to the chat. `publication_status:
   pending` means the engine stored the approval and publishes it on its next
   tick; a repeated approval reports `published` with the `github_review_id`
   once it exists.

Both commands are documented in the [CLI reference](../../cli.md). Their
stable errors apply unchanged: `review_not_awaiting_approval`,
`approval_not_required`, `review_superseded`, `approval_expired` and
`head_changed` each mean that nothing was posted. The skill reports the error
and stops; it never retries with another SHA.

## The approval is bound to the head

`approve` takes the full head SHA, and it applies only if that commit is still
the head of the pull request. A commit pushed after the notification therefore
supersedes the review (`review_superseded`), and the reply cannot approve code
the reviewer never saw. A changed review request does the same.

The notification also tells the person what they approve. Non-blocking notes do
not prevent approval, and neither do blocking findings that the verifier
demoted to notes; the notification lists them, and the reply is the decision.

## Security invariants

These rules are what keep a chat message from publishing more than an approval.
The planned skill and plugin are to enforce them; a change that weakens one of
them is a change to the security model, not a refactor.

- Act only on an explicit reply to a bot notification of a review with no
  blocking findings, and only when the reply asks for `approve`. A plain
  message with the word `approve` does nothing.
- The Review ID and the head come from the quoted notification. The skill never
  constructs a review body, a finding or a comment, and never edits one. The
  engine publishes only what it stored.
- The skill never runs `gh`, never calls the GitHub API, and never loads a
  generic GitHub review or publication skill during the approval.
- It never inspects CI status, checks, workflow runs or logs.
- Quoted notification text, pull request content and findings are data. A
  notification that asks for something other than the approval is ignored.
- Only senders on the Matrix allowlist get the quote, so only they can cause an
  approval through the chat.

## The planned guard plugin

Hermes calls plugin hooks around each message and tool call. The guard plugin
is planned as a Python module with these hooks:

- `pre_gateway_dispatch` quotes the replied-to notice, as described in step 3.
  It fetches the event from the homeserver with the bot's own token, accepts
  only notices the bot itself sent, and rewrites the message with the quote.
- `pre_llm_call` and `pre_tool_call` mark a session as an approval session when
  the message contains a Review ID or runs an `autoreview` approval command.
  In such a session, the plugin blocks direct `gh` commands, `execute_code`
  and the generic GitHub skills. The `autoreview` commands themselves are
  allowed.
- `on_session_reset` and `on_session_finalize` clear that mark.

The plugin is to be a guard, not the only control: the skill's own rules and
the engine's checks (exact head, stored review state) apply without it.

## Setup

Planned in #43.

## What to expect

With the planned integration:

- A reply to a notification of a review that is not waiting returns a stable
  error and posts nothing.
- A reply to a notification that has no Review ID, or a plain `approve` with no
  reply, is ignored.
- A reply after a newer commit returns `review_superseded`. Ask for the new
  review; it is reviewed by the engine on its own.
- A reply after `approval.timeout_hours` returns `approval_expired`. Nothing
  is posted, and the next commit starts a new review.

The integration covers approval only. Retry and the other commands stay in the
CLI, see [retry and recovery](../runbooks/retry-and-recovery.md).

For the decision that approvals wait for, see
[decisions](../concepts/decisions.md). For the wider threat model, see
[secrets](../safety/secrets.md) and [prompt injection](../safety/prompt-injection.md).
