---
type: Concept
id: concept/github-identity
title: GitHub identity
description: What a GitHub credential is (token and login), the one credential the loader accepts, how triggers, own pull requests and approvals follow its login, the startup login check, token kinds and organization approval, and the token permissions each GitHub call needs; several credentials, token checks and expiry warnings are planned.
source_kind: manual
intents: [setup, help, debug]
---

# GitHub identity

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0.

autoreview acts on GitHub through **credentials**. A credential is one GitHub
token, the account (its **login**) that the token belongs to, and the
repositories it serves. Every review, comment, approval and search for review
requests is made with the credential that serves the repository concerned. The
credentials are listed in `github.credentials` in
[config.yaml](../../configuration.md#credentials). The token is never written to
a file by autoreview. It is read from the variable named by `token_env`, in the
`--env-file` or the environment, and each
`gh` child gets only the token of its credential.

## What a credential is

| Field | Meaning |
|---|---|
| `name` | Checked against `[a-z0-9][a-z0-9_-]{0,31}`; not used yet |
| `login` | The account the token belongs to. `serve` checks it at startup |
| `token_env` | The environment variable that holds the token |
| `match` | `["*"]`: the credential serves every repository |

The loader accepts exactly one credential, with `match: ["*"]`, so it serves
every repository of the [policy rules](../../policy.md#repository-rules).

## Choosing the shape

The only shape the loader accepts is one credential with `match: ["*"]`: one
account reviews everything its token can reach. A token per owner, a token per
repository and an account per owner are planned in #13.

## What the login decides

The login of the credential is used in these places, and nowhere else:

| Place | Rule |
|---|---|
| `review-requested` trigger | The search finds pull requests that request a review from the login |
| Drafts of `ready_draft_authors` | A draft that requests a review from the login is marked ready |
| Own pull request | A pull request authored by the login gets a comment, whatever the rule says. See [the own pull request rule](#the-own-pull-request-rule) |
| Answered findings on auto-approve rules | Only a comment by the login, which is not AI-signed, closes a finding. A comment by another account, including the owner's own account under a machine-user credential, does not. See [incremental reviews](incremental-reviews.md#answered-on-auto-approve-rules) |
| Review queue | Lists the pull requests that request a review from the login. See [notifications](notifications.md#review-queue) |

A machine user needs write access on each repository whose rule approves, or the
approval does not count toward branch protection. See
[approvals and branch protection](#approvals-and-branch-protection).

## Startup check and runtime suspension

`autoreview serve` asks GitHub for the token's login before it starts. A token
of another account stops it with `configuration_invalid` naming
`github.credentials.login`, and a token that GitHub refuses stops it with the
error of that call. `validate` makes no GitHub call. A startup check of every
token is planned in #15, and checks while running with the suspension of a
failing credential in #19.

### Expiry

Reporting a token's expiry date is planned in #15, and a warning before the
expiry in #19.

## Token kinds and organization approval

A **fine-grained token** covers one resource owner, a user or an organization. An
organization may require an admin to approve a fine-grained token before it sees
anything, so a new token can review nothing until that approval is given. A
fine-grained token is therefore the narrower choice for a single owner.

A **classic token** can reach every organization its user can access. It is less
granular, and a credential that holds one covers every owner the user reaches,
within its `match`. The `match` limits what the engine asks for, not what the
token can read.

## Permissions required by each GitHub call

The engine uses these GitHub calls. Each one is listed with the permission that
the GitHub documentation assigns to it.

| Engine call | GitHub endpoint or query | Fine-grained permission |
|---|---|---|
| Login check | `GET /user` | None listed; any valid token |
| Review requests and label search | Search issues and pull requests (`gh search prs`, GraphQL search) | Access to the repository; no separate permission listed |
| Pull request read | `GET /repos/{owner}/{repo}/pulls/{number}` and GraphQL pull request query | Pull requests: read |
| Review request events | `GET /repos/{owner}/{repo}/issues/{number}/events` | Pull requests: read, or Issues: read |
| Changed lines | `GET /repos/{owner}/{repo}/pulls/{number}/files` | Pull requests: read |
| Linear history check | `GET /repos/{owner}/{repo}/compare/{previous}...{head}` | Contents: read |
| Existing reviews (idempotency check) | `GET /repos/{owner}/{repo}/pulls/{number}/reviews` | Pull requests: read |
| Publish review | `POST /repos/{owner}/{repo}/pulls/{number}/reviews` | Pull requests: write |
| Discussion | GraphQL pull request discussion (threads and comments) | Not mapped to a row in the docs fetched; read access to the pull request is expected |
| Mark ready for review | GraphQL `markPullRequestReadyForReview` | Not verified in this pass; expected to need Pull requests: write |

Sources: the repository permission table in [Permissions required for fine-grained
personal access tokens](https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens).
In that table the pull request reads (`GET .../pulls/{n}`, `.../files`, `.../reviews`,
`.../issues/{n}/events`) fall under Pull requests: read, and the review
submission (`POST .../pulls/{n}/reviews`) falls under Pull requests: write. The
events row also accepts Issues: read. The search endpoint page
([Search issues and pull requests](https://docs.github.com/en/rest/search/search))
names no permission, only that the caller must be authenticated and have access to
the repositories searched. The [Pull request reviews](https://docs.github.com/en/rest/pulls/reviews)
page did not state the permission for its endpoints. The mutation row is not
verified: the GraphQL reference page fetched did not include it.

### Fine-grained token

- **Pull requests: read and write.** Required. Read covers the calls above, and
  write is required to publish a review.
- **Metadata: read.** Not verified in this pass. Most fine-grained tokens get it by default, and no call above needs more.
- **Issues: read.** Optional. The event and label reads also accept Pull requests
  read, so Issues read is not needed on its own.
- **Contents: read.** Required by the linear history check, the compare call in the
  table above. Without it, a re-review cannot be marked incremental (see
  [incremental reviews](incremental-reviews.md#full-or-incremental)).
- **Single resource owner.** A fine-grained token is limited to one user or
  organization. The [managing personal access tokens](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)
  page states: "Each token is limited to access resources owned by a single user or
  organization." A fine-grained token therefore covers one owner; several
  credentials for several owners are planned in #13.

### Classic token

- **`repo`** grants full access to public and private repositories, including read
  and write access to code. Use it when the rules cover private repositories.
- **`public_repo`** limits access to public repositories. Use it when every rule
  matches public repositories only.

Source: [Scopes for OAuth apps](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/scopes-for-oauth-apps).
That page does not describe pull request or review permissions in detail, so the
scopes above are the ones that cover repository access in general.

### Review hosts fetch code with their own credentials

The review hosts fetch the repository with their own git credentials, not with
these tokens. A private repository therefore needs read access on every review
host that runs its reviews, in addition to the token of its credential.

## Repositories of several owners

Planned in #13.

## Approvals and branch protection

An approval counts toward a branch protection rule only when it comes from an
account with write permission on the repository, or from a designated code owner.
The protected branches page says: "You can require approving reviews from people
with write permissions in the repository or from a designated code owner." (see
[About protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches)).

The consequence for autoreview: the login of the credential that posts the
`APPROVE` review must have write permission on the repository, or the approval does
not count toward a required approval count. Give the credential's login write
permission on every repository whose rule uses `on_clean: approve` or
`on_clean: await`.

## The own pull request rule

A pull request that the reviewing account authored gets `COMMENT`, whatever the
rule says. The reviewing account is the login of the credential that serves the
repository. GitHub does not accept an approval or a request for changes from the
author of a pull request on that pull request. autoreview checks the author
itself, so the outcome does not depend on GitHub's refusal. The check also clears
any approval wait for such a pull request. Details are in
[decisions: own pull requests](decisions.md#own-pull-requests).

A GitHub-side rule that forbids the author to approve the own pull request is
widely known, but the reviews API page fetched for this page does not state it.
The rule is therefore cited here as the engine's rule, not as a GitHub guarantee.

## Why not a GitHub App yet

The project decision for v0.1 is a personal token or a machine user (decision D5 of
the project plan), with a GitHub App as a later phase. Two points are open:

- **Review requests.** The trigger `review-requested` asks whether the pull request
  requests a review from the credential's login. Review requests are made to users
  and teams. Whether a GitHub App identity can be requested as a reviewer is an
  open question that the plan lists for phase 6. This page does not verify the
  GitHub documentation on it.
- **Label trigger.** Label triggers work under any identity that can read the
  pull request, so a GitHub App would support them without that question being
  answered.

Until that question is answered, `review-requested` needs a user identity, and v0.1
keeps tokens.

## Handling the tokens

- Put the token in the environment under the name given by its `token_env`. The
  name must match `AUTOREVIEW_[A-Z0-9_]+`. The install of the service sets the
  environment, see [install](../../install.md).
- Check that a token is set without printing it, using the name from
  `config.yaml`: `test -n "$AUTOREVIEW_GH_TOKEN" && echo set`.
- Each `gh` child gets the token as `GH_TOKEN` in its own
  environment. No other process that autoreview starts receives a token; the review
  agents run with the pohunek daemon's environment, see
  [secrets](../safety/secrets.md).
- A token that is pasted into a chat, a log or a configuration file must be
  rotated. Rotating one credential does not affect the others. See
  [secrets](../safety/secrets.md).

## Related pages

- The configuration keys for credentials: [configuration: credentials](../../configuration.md#credentials).
- Who can open a review and how author classes work: [untrusted authors](../safety/untrusted-authors.md).
- Setting up a machine user: [solo maintainer](../guides/solo-maintainer.md).
- What a token can reach: [threat model](../safety/threat-model.md).
