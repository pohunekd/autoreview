---
type: Safety
id: safety/threat-model
title: Threat model
description: What autoreview protects, who can act on it, what runs where, the attacks it is designed against with their mitigations, and the residual risks an operator must accept.
source_kind: manual
intents: [setup, help]
---

# Threat model

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

autoreview reads pull requests written by people it may not trust, sends them
to coding agents, and then posts a review under a GitHub account. Its threats
come from that combination. This page lists the assets and actors, shows where
each part runs, and gives the attacks with their mitigations. The mitigations
are design commitments of v0.1; the residual risks at the end are not solved by
autoreview.

Related: [untrusted authors](untrusted-authors.md), [prompt injection](prompt-injection.md), [secrets](secrets.md).

## Assets

| Asset | Why it matters | Where it lives |
|---|---|---|
| GitHub tokens, one per credential | Each lets the service read pull requests and post reviews and approvals as its login, in the repositories its `match` covers | Environment of the autoreview host, named by `token_env`; passed only to the `gh` calls of those repositories |
| Approvals of the credentials' accounts | An approval can satisfy branch protection, so it is the most valuable outcome | GitHub; published by the engine |
| Review host | Runs reviewer agents in worktrees, with the code and the agent's credentials | Each pohunek review host |
| Reviewer model accounts | Keys or logins of the models that review code and cost money | The runtime login on the review host, not the pohunek profile |
| Backend secrets | Matrix tokens and webhook URLs, which are secrets for most services | Environment, named by `*_env` keys |
| Store | Review history, discovery state, published decisions | SQLite file at `store.path` |

## Actors

| Actor | Trust | What they control |
|---|---|---|
| Operator | Trusted | `config.yaml`, `policy.yaml`, prompts, the environment file, the review hosts |
| Trusted author | Trusted by the rule | Pull requests the rule accepts from them; can still write bad code |
| Pull request author, untrusted | Not trusted | The diff, the description, the commit messages, files in the head |
| Fork contributor | Not trusted unless listed in `logins` | Everything an untrusted author controls, from a fork |
| Other comment authors | Not trusted | Comments and review threads, which are read as discussion of earlier findings |
| Reviewer model | Untrusted output | The text it returns; it can be steered by the content it reads |
| GitHub | Trusted platform | Stores the reviews; its search results bound discovery |

## What runs where

| Component | Host | Holds | Reaches |
|---|---|---|---|
| `autoreview` service | Autoreview host | GitHub tokens, backend secrets, the store | GitHub through `gh`; pohunek CLI; notifier backends |
| `gh` | Autoreview host | One credential's token, for the calls of its repositories | GitHub API |
| `pohunek` CLI | Autoreview host | Its own state and runtime directories, no GitHub token | Review host daemons |
| pohunek daemon | Review host | Sessions, worktrees, project setup | Git remote, the agent runtime |
| Reviewer session (`claude`, `codex`) | Review host, in a worktree | Model login, the code in the checked-out ref | Model API only; read-only run |
| Verifier session | Review host, in a worktree | Same as reviewer | Same as reviewer |

Each `gh` child gets only the token of its credential, as `GH_TOKEN` in its own environment. No GitHub token is in a reviewer session, and the reviewer returns a
structured result, and only the engine publishes.

## Attacks and mitigations

| # | Attack | Mitigation | Status |
|---|---|---|---|
| 1 | Untrusted code runs in the reviewer's worktree | Untrusted authors and forks are reviewed from `checkout: base`, so their code is not checked out as the working tree | Designed; checked by a planned test |
| 2 | Untrusted author gets an auto-approval | Untrusted sections may use `on_clean: comment` or `await`, never `approve`; `validate` rejects `approve` there | Designed; `validate` rule |
| 3 | Prompt injection through the diff, description, comments or files | Prompts declare PR content as data; reviewer runs are read-only; the engine validates the output and publishes it | See [prompt injection](prompt-injection.md) |
| 4 | Model output forges the review marker or pings people | Output is neutralized: no `@mentions`, no raw HTML, no images that GitHub would fetch, HTML comment delimiters defused | Engine behaviour |
| 5 | Reviewer reads a GitHub token | Tokens are passed only to the `gh` calls of their repositories; child processes never inherit one or pohunek session identity variables | Engine behaviour |
| 6 | Reviewer reads backend secrets or pohunek profile secrets | Keep secrets out of the profile; planned: adapters refuse known secret variables | Partly designed; see [secrets](secrets.md) |
| 7 | Wrong account posts reviews | `serve` refuses to start when a token's login differs from its credential's login; a token that fails while running suspends only its own credential | Engine behaviour |
| 8 | Own pull request gets approved or changes requested | GitHub forbids it, so the engine always posts a comment and raises `findings_on_own_pr` | Policy behaviour |
| 9 | Approval satisfies branch protection for a head nobody read | An approval binds to the exact head SHA; a new commit or changed request supersedes it | Policy behaviour |
| 10 | Approval spoofed by a comment | Approval is a CLI command, not a comment; comment-based approval is post-v1 | Not in v0.1 |
| 11 | Human comment reused to close a finding on an auto-approve rule | Comments marked by `publish.ai_signed_pattern` are never a human decision; on auto-approve rules a finding closes as answered only when it rests on the reviewing account's own comment that is not AI-signed | Engine behaviour |
| 12 | Anyone with triage rights triggers a costly run | Use `review-requested` or a label that only maintainers can add; untrusted authors are skipped or reviewed with `comment` | Policy choice; per-author rate limit not in v0.1 |
| 13 | Discovery hides pull requests when a search is truncated | Truncated search is an error (`github_search_limit_exceeded`), never a silent drop | Engine behaviour |
| 14 | A reviewer returns oversized or malformed output | Schema-validated result, `limits` bounds; the reviewer gets a fresh session | Engine behaviour |
| 15 | Two engines process one database | `serve` refuses a second process on the same database (`engine_already_running`) | Engine behaviour |
| 16 | Secret written into config, policy or a prompt | Only `*_env` names appear in config; secret values stay in the environment file | Operator rule |

## Residual risks

These are not solved by autoreview. Operators must accept or mitigate them.

- **pohunek project setup and hooks run on the worktree.** They run in every
  session's worktree. With `checkout: head`, that worktree holds the pull
  request's code, so its hooks are code the author controls. Run reviews on a
  dedicated review host with a project that has no setup hooks. Planned: an
  option to require a hook-free project, not in v0.1.
- **The head checkout runs nothing by itself, but the agent reads it.** For
  `checkout: head`, the agent reads untrusted files. Use `checkout: base` for
  every rule that accepts outside authors.
- **Model mistakes.** A reviewer can miss a bug or invent one. The verifier
  reduces false blocking findings and the human approval step catches the rest,
  but neither is a guarantee.
- **Trusted authors can still write harmful code.** Trust decides what is
  reviewed and approved automatically; it does not check the code.
- **Token scope.** A token with more rights than posting reviews can do more
  than the engine needs. Use the narrowest scopes that work, see
  [GitHub identity](../concepts/github-identity.md).
- **One host holds every token.** The autoreview host holds one token per
  credential, so a compromised host exposes all of them. A leaked token exposes
  only the repositories of its credential, see
  [secrets](secrets.md#blast-radius-of-a-token).
- **Model accounts and costs.** Reviews spend the model accounts on the review
  hosts. Limit who can trigger a review.
- **Notifier channels.** Matrix rooms and chat webhooks receive review text.
  Choose rooms and channels with the same audience as the repository.

## Recommended host isolation

- Separate the autoreview host from the review hosts. The review host holds
  the agents and the code, the autoreview host holds the token.
- Give each review host only the repositories it reviews, and register them with
  `pohunek project add` only on the hosts that should see them.
- Run untrusted-author reviews on a review host without secrets in its
  environment, as far as the host allows. v0.1 chooses the host per repository
  with `route_overrides`; see [untrusted authors](untrusted-authors.md#separate-review-host).
