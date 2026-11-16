---
type: Safety
id: safety/threat-model
title: Threat model
description: What autoreview protects, who can act on it, what runs where, the attacks it is designed against with their mitigations, and the residual risks an operator must accept.
source_kind: manual
intents: [setup, help]
---

# Threat model

> **Status: after 0.6.0.** This page describes autoreview on `main` after the
> 0.6.0 release. A part that is not available names its open issue.

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
| Store | Review history, discovery state, published decisions, and daily counts per repository, rule, reviewer and error code that outlive the review retention; one outcome row per published finding (repository, path, line, severity, no finding text) for `store.outcome_retention_seconds` | SQLite file at `store.path` |
| Metrics text | Counts and the names of repositories, review hosts, rules and error codes | Served by `serve` on `metrics.listen` when the `metrics` section is set |
| MCP bearer tokens | A `write` token can approve a waiting review and start a discovery pass; a `read` token reads review text and runs GitHub searches | Held by each MCP client; the configuration holds only their SHA-256 digests (`mcp.clients[N].token_sha256`). Rotate a token by replacing its digest and restarting `autoreview mcp` |

## Actors

| Actor | Trust | What they control |
|---|---|---|
| Operator | Trusted | `config.yaml`, `policy.yaml`, prompts, the environment file, the review hosts |
| Trusted author | Trusted by the rule | Pull requests the rule accepts from them; can still write bad code |
| Pull request author, untrusted | Not trusted | The diff, the description, the commit messages, files in the head |
| Fork contributor | Not trusted unless listed in `logins` | Everything an untrusted author controls, from a fork |
| Other comment authors | Not trusted | Comments and review threads, which are read as discussion of earlier findings and, on a rule with `guard`, scanned by the text guard |
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
| 3 | Prompt injection through the diff, description, comments or files | Prompts declare PR content as data; reviewer runs are read-only and keep out the MCP servers of the host's agent configuration (Claude Code runs with `--strict-mcp-config` and no `--mcp-config`, Codex with `--ignore-user-config`), so injected text reaches no mail, chat or autoreview MCP tool registered there; Codex reads no `AGENTS.md` of the checkout (`project_doc_max_bytes=0`), lists no skills catalog (`skills.include_instructions=false`), and the Codex adapter refuses a `checkout: head` run whose skill folders on disk differ from the trusted base (`repository_skills`); the engine validates the output and publishes it | See [prompt injection](prompt-injection.md); adapter tests `test_claude_arguments_end_with_the_fixed_read_only_tail`, `test_codex_arguments_are_the_fixed_read_only_command`, `test_head_checkout_refuses_skills_that_codex_would_load` and `test_head_checkout_refuses_a_skill_that_the_run_leaves_on_disk` |
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
| 20 | Hidden text in the pull request steers a reviewer into a clean verdict on an `on_clean: approve` rule | A rule with `guard` scans the fields it names before any reviewer runs, without a model and without sending the text anywhere. A suspect pull request (its score reaches the threshold, or the read left text out) with `on_suspect: await` waits for a person instead of being approved, and every suspect result sends a `guard_suspect` message with signal codes only | Engine behaviour, opt-in per rule; tests `text_guard_holds_a_clean_approval_and_never_blocks` and `text_guard_holds_a_suspect_approval_and_log_changes_nothing`; see [prompt injection](prompt-injection.md#the-text-guard) |
| 21 | A `checkout: head` pull request adds or changes a pohunek hook (`.pohunek/hooks/<event>`, `.pohunek/setup`) or a git hook, so that its code runs on the review host | Every head-checkout session starts with `pohunek session new --no-repo-hooks`: pohunek runs none of the checkout's own hooks and pins git's hooks and fsmonitor off, for the session's whole life (pohunek 0.43.1 or later, CLI and daemon). Before each start autoreview reads `host inspect` and refuses the review when the host does not advertise the switch (`review_host_repo_hooks_unsupported`) or, under `pohunek.head_checkout_host_hooks: refuse`, when it reports anything but `false` for an event other than `pre_create` and `post_remove`, also for an event that a later pohunek adds, or no `host_hooks` at all (`review_host_hooks_refused`); no session starts then | Engine behaviour; tests `head_checkout_starts_only_where_the_host_skips_the_checkouts_hooks`, `head_checkout_starts_only_without_the_checkouts_hooks` and `head_checkout_hooks_are_only_an_advertised_switch_and_explicit_false`. A session that an earlier release started keeps running without the switch until it ends |

## Residual risks

These are not solved by autoreview. Operators must accept or mitigate them.

- **A head checkout still runs the host's own hook scripts and git drivers.**
  The checkout's own hooks never run (attack 21), but pohunek's host-global
  scripts (`hooks/<event>` in its config directory) run in the worktree for
  `post-create`, `pre-remove` and the session events, and the agent's git
  applies the filter, textconv and diff drivers that the host's git
  configuration names. A script or driver that runs project tooling (an
  install, a build, a path relative to the checkout) executes the pull
  request's code. `pohunek.head_checkout_host_hooks: refuse` refuses a head
  checkout on a host that reports such a script; `allow` accepts the risk.
  With `checkout: base`, the worktree is the base branch tip, so the head's
  tooling never runs; every untrusted author that a rule reviews gets it. A
  head checkout runs only for the authors that `authors.trusted` names, so on
  a rule whose pipeline uses `checkout: head`, trust only authors who may run
  code on the review host, and never use `trusted: any` on a repository with
  outside contributors; use `checkout: base` for everyone else.
- **Apart from the host's own scripts, the head checkout runs nothing, but the
  agent reads it.** For `checkout: head`, the agent reads untrusted files. Use
  `checkout: base` for every rule that accepts outside authors.
- **A Codex run can start a process outside the adapter's process tree.** The
  Codex adapter ends only the processes below it (attack 19). A tool command
  that has another service start a process, for example a user service
  manager over its socket, leaves one that the adapter does not end. If that
  process can read the session output and write to the session's terminal
  device, it can append a tail with the nonce after the adapter's markers.
  Whether the read-only Codex sandbox allows both is not verified. Run Codex
  reviews on a dedicated review host whose user runs no other services.
- **A Codex run trusts the project configuration that the host trusts.**
  Codex 0.167.0 reads a `.codex/config.toml` in the checkout, which may add MCP
  servers, only for a trusted project. Under `--ignore-user-config` only the
  host's system or managed configuration can mark it trusted, never the
  checkout, and the adapter passes `project_doc_max_bytes=0`, so no `AGENTS.md`
  of the checkout becomes an instruction (#449, see
  [the Claude adapter](../concepts/reviewers-and-adapters.md#the-claude-adapter)
  section, which covers the MCP servers of both adapters).
  A host configuration that trusts a review worktree or its repository gives a
  `checkout: head` pull request that file. Another Codex version may load it
  differently; autoreview does not pin one.
- **A Codex run loads the repository skills that the trusted base has.**
  Codex 0.167.0 loads the skills under `.agents/skills/` and `.codex/skills/`
  of the checkout without host trust and injects the whole `SKILL.md` of a
  skill that the prompt names; no Codex setting turns that off. The adapter
  drops the skills catalog and refuses a `checkout: head` run whose skill
  folders on disk differ from the trusted base tree (#454). It compares every
  entry there with the base itself, before Codex starts and again after the
  run, so content that head code hides from git with an index flag, an ignore
  rule or a nested repository is refused too, and a skill that the run leaves
  on disk fails the run. A skill that a process of the head writes during the
  run and removes before it ends passes both checks: that needs head code
  running on the review host, which is the residual risk of the host's own
  hook scripts and git drivers (above), not a bypass of its own. It also
  refuses a head whose skill folders hold a symbolic link or a submodule
  (gitlink), even one that the base has, a `.git` entry, a file whose
  checked-out bytes differ from the base blob (for example through a
  `.gitattributes` filter), a file or folder whose name is not ASCII when
  Python's file system encoding on the review host is not UTF-8, or skill
  folders of more than 10000 entries in all, and a head that still has the
  older copy of a skill that the base branch changed after the fork, until the
  pull request merges or rebases onto the base branch. Another Codex version
  may load skills from other places; autoreview does not pin one.
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
- **The text guard catches hidden text, not every injection.** An instruction
  in visible prose scores only by the lexicon; one below the threshold, in a
  file that the pull request does not change, or in an image is not found. A
  rule without `guard`, or with `on_suspect: log` or `comment`, still approves
  a clean review automatically. Enable it as
  [the guide](../guides/text-guard.md) describes and keep the verifier and the
  `escalate_*` settings on approving rules.

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
  before the in-repo ones: in the session's worktree for `post-create`,
  `pre-remove` and the session events, and in the repository for `pre-create`
  and `post-remove`, which never see the pull request's code. On a host that
  serves `checkout: head` reviews, a global hook that runs project tooling,
  such as an install or a build, runs the pull request's code. Keep
  `pohunek.head_checkout_host_hooks: refuse` there unless every such script
  runs nothing from the checkout.
