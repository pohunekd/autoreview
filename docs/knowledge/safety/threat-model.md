---
type: Safety
id: safety/threat-model
title: Threat model
description: What autoreview protects, who can act on it, what runs where, the attacks it is designed against with their mitigations, and the residual risks an operator must accept.
source_kind: manual
intents: [setup, help]
---

# Threat model

> **Status: after 0.3.0.** This page describes autoreview on `main` after the
> 0.3.0 release. A part that is not available names its open issue.

autoreview reads pull requests written by people it may not trust, sends them
to coding agents, and then posts a review under a GitHub account. Its threats
come from that combination. This page lists the assets and actors, shows where
each part runs, and gives the attacks with their mitigations. The mitigations
are what autoreview does; the residual risks at the end are not solved by
autoreview.

Related: [untrusted authors](untrusted-authors.md), [prompt injection](prompt-injection.md), [secrets](secrets.md).

## Assets

| Asset | Why it matters | Where it lives |
|---|---|---|
| GitHub token of the credential | It lets the service read pull requests and post reviews and approvals as its login, wherever its scope reaches | Environment of the autoreview host, named by `token_env`; passed only to its `gh` children |
| Approvals of the credential's account | An approval can satisfy branch protection, so it is the most valuable outcome | GitHub; published by the engine |
| Review host | Runs reviewer agents in worktrees, with the code and the agent's credentials | Each pohunek review host |
| Reviewer model accounts | Keys or logins of the models that review code and cost money | The runtime login on the review host, not the pohunek profile |
| Backend secrets | The Matrix token of the notifier | Environment, named by `*_env` keys |
| Store | Review history, discovery state, published decisions, and daily counts per repository, rule, reviewer and error code that outlive the review retention | SQLite file at `store.path` |
| Metrics text | Counts and the names of repositories, review hosts, rules and error codes | Served by `serve` on `metrics.listen` when the `metrics` section is set |
| MCP bearer tokens | A `write` token can approve a waiting review and start a discovery pass; a `read` token reads review text and runs GitHub searches | Held by each MCP client; the configuration holds only their SHA-256 digests (`mcp.clients[N].token_sha256`). Rotate a token by replacing its digest and restarting `autoreview mcp` |

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
| MCP client with the `write` scope | Semi-trusted | `resolve`, `approve` and `retry`, and the read tools. An agent client reads review text that pull request authors and models wrote, so injected text can steer it |
| MCP client with the `read` scope | Semi-trusted | The read tools: review text, statuses, the review queue and the review statistics |
| Anyone who reaches `metrics.listen` | Not trusted | `GET /metrics`: the metrics text, no review text and no write |

## What runs where

| Component | Host | Holds | Reaches |
|---|---|---|---|
| `autoreview` service | Autoreview host | GitHub tokens, backend secrets, the store | GitHub through `gh`; pohunek CLI; notifier backends |
| `autoreview mcp` | Autoreview host, behind a TLS reverse proxy | The token digests, the store (same file as the service), the engine credential for the GitHub calls of `review_queue` and `retry` | GitHub through `gh`; MCP clients through the proxy |
| Metrics endpoint of `serve` (optional) | Autoreview host, on `metrics.listen`, loopback recommended | The review counters of the process; it reads the store | The scraper, directly or through a reverse proxy |
| `gh` | Autoreview host | One credential's token, for the calls of its repositories | GitHub API |
| `pohunek` CLI | Autoreview host | Its own state and runtime directories, no GitHub token | Review host daemons |
| pohunek daemon | Review host | Sessions, worktrees, project setup | Git remote, the agent runtime |
| Reviewer session (`claude`, `codex`) | Review host, in a worktree | Model login, the code in the checked-out ref | Model API only; read-only run |
| Verifier session | Review host, in a worktree | Same as reviewer | Same as reviewer |

Each `gh` child gets only the token of its credential, as `GH_TOKEN` in its own environment.
autoreview passes no GitHub token to a reviewer session, whose environment comes from the
pohunek daemon (see [secrets](secrets.md)); the reviewer returns a structured result, and only
the engine publishes.

## Attacks and mitigations

| # | Attack | Mitigation | Status |
|---|---|---|---|
| 1 | Untrusted code runs in the reviewer's worktree | Untrusted authors and forks are reviewed from `checkout: base`, so their code is not checked out as the working tree | Engine behaviour; test `untrusted_input_needs_a_base_checkout_and_no_approval` |
| 2 | Untrusted author gets an auto-approval | Untrusted sections may use `on_clean: comment` or `await`, never `approve`; `validate` rejects `approve` there, and publication never approves an untrusted author without the user | `validate` rule and engine behaviour; test `clean_review_of_an_untrusted_author_is_never_approved_without_the_user` |
| 3 | Prompt injection through the diff, description, comments or files | Prompts declare PR content as data; reviewer runs are read-only; the engine validates the output and publishes it | See [prompt injection](prompt-injection.md) |
| 4 | Model output forges the review marker or pings people | Output is neutralized: no `@mentions`, no raw HTML, no images that GitHub would fetch, no references to another repository's issues, pull requests or discussions, no link reference definitions, HTML comment delimiters defused | Engine behaviour |
| 5 | Reviewer reads a GitHub token | Tokens are passed only to the `gh` calls of their repositories, as `GH_TOKEN`; the `gh` and `pohunek` processes autoreview starts inherit no token variable and no pohunek session identity variables | Engine behaviour |
| 6 | Reviewer reads backend secrets or pohunek profile secrets | The `gh` and `pohunek` processes autoreview starts inherit only an allowlist of variables, never an `AUTOREVIEW_*` variable or an `*_env` name. Review agents run with the pohunek daemon's environment, which autoreview does not control: keep secrets out of it and out of the profile | Engine behaviour for autoreview's processes; the daemon and the profile are the operator's; see [secrets](secrets.md#what-child-processes-receive) |
| 7 | Wrong account posts reviews | `serve` refuses to start when the token's login differs from the credential's login; a review reads and publishes only with the credential and login it was dispatched with; a check while running that finds another account suspends the credential | Engine behaviour |
| 8 | Own pull request gets approved or changes requested | GitHub forbids it, so the engine always posts a comment and raises `findings_on_own_pr` | Policy behaviour |
| 9 | Approval satisfies branch protection for a head nobody read | An approval binds to the exact head SHA; a new commit or changed request supersedes it | Policy behaviour |
| 10 | Approval spoofed by a comment | Approval is the `approve` CLI command or the `approve` tool of an MCP client with the `write` scope, never a comment; both check the exact head. Comment-based approval is post-v1 (#46) | Engine behaviour |
| 11 | Human comment reused to close a finding on an auto-approve rule | Comments marked by `publish.ai_signed_pattern` are never a human decision; on auto-approve rules a finding closes as answered only when it rests on the reviewing account's own comment that is not AI-signed | Engine behaviour |
| 12 | Anyone with triage rights triggers a costly run | Use `review-requested` or a label that only maintainers can add; untrusted authors are skipped or reviewed with `comment`. A rule's `author_limit` bounds the reviews started for one pull request author in a window and waits or skips over it; it counts per author, not per person who adds the label or requests the review | Policy choice; `author_limit` is engine behaviour, test `author_limit_counts_every_start_of_an_author_once` |
| 13 | Discovery hides pull requests when a search is truncated | Truncated search is an error (`github_search_limit_exceeded`), never a silent drop | Engine behaviour |
| 14 | A reviewer returns oversized or malformed output | Schema-validated result, `limits` bounds, `session.output_max_bytes`; the reviewer gets a fresh session, or the review fails at once for an output over the bound (`review_output_too_large`) and a Claude Code result message that is unusable (`claude_result_invalid`, `claude_result_mismatch`) | Engine behaviour |
| 15 | Two engines process one database | `serve` refuses a second process on the same database (`engine_already_running`) | Engine behaviour |
| 16 | Secret written into config, policy or a prompt | Only `*_env` names appear in config; secret values stay in the environment file | Operator rule |
| 17 | An agent with an MCP `write` token is steered into `approve` or `retry` by text injected into a pull request | Tool results carry review text as neutralized data inside an untrusted-data notice; `approve` publishes only a clean review that waits at the exact head. Give the `write` scope only to agents that keep review text out of their instructions | Operator choice; see the [MCP server guide](../guides/mcp-server.md#untrusted-output) |
| 18 | Another process, a host on the network or a web page reads or floods the metrics endpoint | The endpoint is off without its `metrics` section and checks no token, so keep `listen` on loopback and put a reverse proxy with authentication in front of a scraper on another host. It serves only `GET /metrics`, only for a `Host` in `allowed_hosts` (against DNS rebinding), and refuses any request with an `Origin` header. `max_connections` bounds the connections at once, `header_timeout_seconds` the time to send a request head, and `request_timeout_seconds` each connection, which serves one request, and the wait for them at shutdown; each scrape is one indexed read of the store. The text names repositories, hosts, rules and error codes, which a reader learns | Engine behaviour; `listen` and the proxy are the operator's |
| 19 | A Codex transcript forges the adapter's result frame and exit line, printed by the model or by tool output steered from the pull request, while the model pauses | Every Codex marker carries a nonce that the engine draws from the OS random source for each run; the adapter removes the nonce line before Codex reads the prompt, and the engine reads only the markers with the run's nonce. Before the adapter prints a marker, it ends every process that the Codex run left (it is their reaper, so a double fork or a new session does not escape) and stops copying Codex's stderr, so no process of the run can append a tail once the nonce is in the output | Engine and adapter behaviour; tests `codex_output_is_read_only_with_the_markers_of_the_request_nonce` and `test_a_process_that_codex_leaves_behind_cannot_write_after_the_markers`; a process started outside the adapter's process tree is a residual risk. An adapter from an earlier release refuses such a request before Codex runs, so the nonce never reaches the model; install the profiles before the engine |

## Residual risks

These are not solved by autoreview. Operators must accept or mitigate them.

- **pohunek runs the hooks of the review worktree.** pohunek reads the in-repo
  hooks (`.pohunek/hooks/<event>`, and `.pohunek/setup` for `post-create`) from
  the session's worktree and runs them as its daemon user for `post-create`,
  `session-start`, `session-stop`, `agent-state` and `pre-remove`. With
  `checkout: head`, that worktree holds the pull request's code, so every
  in-repo hook, including one the pull request adds, and any project tooling
  that a hook or a host-global hook runs (an install, a build, a script in the
  repository) executes the pull request's code, even when the project's own
  repository has no hooks. With `checkout: base`, the worktree is the base
  branch tip, so the head's hooks and tooling never run; every untrusted author
  that a rule reviews gets it. A head checkout runs only for the authors that
  `authors.trusted` names, so on a rule whose pipeline uses `checkout: head`,
  trust only authors who may run code on the review host, and never use
  `trusted: any` on a repository with outside contributors; use
  `checkout: base` for everyone else. A pohunek switch that skips the in-repo
  hooks is requested in
  [zajca/pohunek#2366](https://github.com/zajca/pohunek/issues/2366), and #399
  tracks the autoreview option that uses it.
- **Apart from its hooks, the head checkout runs nothing, but the agent reads
  it.** For `checkout: head`, the agent reads untrusted files. Use
  `checkout: base` for every rule that accepts outside authors.
- **A Codex run can start a process outside the adapter's process tree.** The
  Codex adapter ends only the processes below it (attack 19). A tool command
  that has another service start a process, for example a user service
  manager over its socket, leaves one that the adapter does not end. If that
  process can read the session output and write to the session's terminal
  device, it can append a tail with the nonce after the adapter's markers.
  Whether the read-only Codex sandbox allows both is not verified. Run Codex
  reviews on a dedicated review host whose user runs no other services.
- **Model mistakes.** A reviewer can miss a bug or invent one. The verifier
  reduces false blocking findings and the human approval step catches the rest,
  but neither is a guarantee.
- **Trusted authors can still write harmful code.** Trust decides what is
  reviewed and approved automatically; it does not check the code.
- **Token scope.** A token with more rights than posting reviews can do more
  than the engine needs. Use the narrowest scopes that work, see
  [GitHub identity](../concepts/github-identity.md).
- **The host holds the token.** A compromised autoreview host exposes the
  token and everything its scope reaches, see
  [secrets](secrets.md#blast-radius-of-a-token).
- **Model accounts and costs.** Reviews spend the model accounts on the review
  hosts. Limit who can trigger a review.
- **Notifier channels.** Matrix rooms receive review text.
  Choose rooms and channels with the same audience as the repository.

## Recommended host isolation

- Separate the autoreview host from the review hosts. The review host holds
  the agents and the code, the autoreview host holds the token.
- Give each review host only the repositories it reviews, and register them with
  `pohunek project add` only on the hosts that should see them.
- Run untrusted-author reviews on a review host without secrets in its
  environment, as far as the host allows. autoreview chooses the host per repository
  with `route_overrides`, and the host of the untrusted authors with the entry's
  `untrusted` route, which never falls back to the trusted host; see
  [untrusted authors](untrusted-authors.md#separate-review-host).
- pohunek's host-global hooks (`hooks/<event>` in its config directory) run
  before the in-repo ones, with the session's worktree as their working
  directory. On a host that serves `checkout: head` reviews, a global hook that
  runs project tooling, such as an install or a build, runs the pull request's
  code.
