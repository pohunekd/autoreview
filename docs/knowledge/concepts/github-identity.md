---
type: Concept
id: concept/github-identity
title: GitHub identity
description: What a GitHub credential is (token, login and the repositories it serves), single token versus per-owner, per-repository and different-account credentials, how triggers, own pull requests, signatures and the review queue follow a login, the startup check versus runtime suspension, expiry warnings, organization approval, and the token permissions each GitHub call needs.
source_kind: manual
intents: [setup, help, debug]
---

# GitHub identity

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

autoreview acts on GitHub through **credentials**. A credential is one GitHub
token, the account (its **login**) that the token belongs to, and the
repositories it serves. Every review, comment, approval and search for review
requests is made with the credential that serves the repository concerned. The
credentials are listed in `github.credentials` in
[config.yaml](../../configuration.md#credentials). The token is never written to
a file. It is read from the environment variable named by `token_env`, and each
`gh` child gets only the token of its credential.

## What a credential is

| Field | Meaning |
|---|---|
| `name` | Shown in logs and notifications |
| `login` | The account the token belongs to. The engine checks it at startup |
| `token_env` | The environment variable that holds the token |
| `match` | The repositories the credential serves: `owner/repository`, `owner/*` or `*` |

Each repository is served by exactly one credential: the first credential whose
`match` covers it. Every policy rule must be covered by a credential
(`credential_not_covered` otherwise), and every credential must serve at least
one rule (`credential_unused` otherwise). The rules and their order are in
[policy: repository rules](../../policy.md#repository-rules).

## Choosing the shape

| Shape | Configuration | Use when |
|---|---|---|
| Single token | One credential with `match: ["*"]` | One account reviews everything it can reach. `autoreview init` writes this shape |
| Token per owner | One fine-grained credential for each owner, with `match: [owner/*]` | Each owner grants its own token, and a token only ever covers one owner |
| Token per repository | A credential for `owner/repository`, listed above the owner's credential | One repository needs a narrower token than its owner's |
| Different account per owner | Credentials for the same or different logins, one per owner | Reviews of one owner should appear from another account, for example a machine user for a public organization and your own account for private repositories |

Several credentials may share a login. The login decides who the reviews appear
from and what the reviewer is asked for, so the shapes differ in three places:
the tokens (how much a leak exposes), the accounts (who the reviews appear from),
and the rules (which login a trigger asks for).

### Worked example

Repositories of `zajca` are reviewed from your own account, and repositories of
`pohunekd` from a machine user. Each credential serves a rule, and nothing
follows a `*`:

```yaml
credentials:
  - name: personal
    login: zajca
    token_env: AUTOREVIEW_GH_TOKEN_ZAJCA
    match: [zajca/*]
  - name: pohunekd-bot
    login: pohunekd-review
    token_env: AUTOREVIEW_GH_TOKEN_POHUNEKD
    match: [pohunekd/*]
```

With these credentials:

- A `review-requested` rule for `zajca/*` finds requests made to `zajca`, and the
  reviews are published and approved as `zajca`.
- A `label:ai:review` rule for `pohunekd/*` reads the label, and the reviews are
  published and approved as `pohunekd-review`.
- A pull request in `zajca/*` that `zajca` authored is an own pull request and
  gets a comment. A pull request in `pohunekd/*` authored by `zajca` is not an own
  pull request, because `pohunekd-review` is the reviewing account there, so it can
  receive an approval from that account, if the account has write access.
- The signature names the login of the credential that served the review.

To give one repository its own token, put a credential with
`match: [zajca/pohunek]` above the `zajca/*` credential. The first match wins.

## What the login decides

The login of the serving credential is used in these places, and nowhere else:

| Place | Rule |
|---|---|
| `review-requested` trigger | The search finds pull requests that request a review from the login. Searches run per credential and only within its `match` |
| Drafts of `ready_draft_authors` | A draft that requests a review from the login is marked ready |
| Own pull request | A pull request authored by the login gets a comment, whatever the rule says. See [the own pull request rule](#the-own-pull-request-rule) |
| Signature | `{login}` in `publish.signature` is replaced by the login |
| Answered findings on auto-approve rules | Only a comment by the login, which is not AI-signed, closes a finding. A comment by another account, including the owner's own account under a machine-user credential, does not. See [incremental reviews](incremental-reviews.md#answered-on-auto-approve-rules) |
| Review queue | Grouped by account when the credentials have different logins. See [notifications](notifications.md#review-queue) |

A machine user needs write access on each repository whose rule approves, or the
approval does not count toward branch protection. See
[approvals and branch protection](#approvals-and-branch-protection).

## Startup check and runtime suspension

`autoreview serve` and `autoreview validate` check every credential before they
do anything else. For each token, the check reads the token's login and its
expiry date, and then:

- A token that is rejected, expired, or belongs to another login than its
  credential stops startup (`credential_invalid`, `credential_login_mismatch`).
  The service does not start while any credential is broken.
- While the service runs, each token is checked every
  `github.credential_check_hours`, and GitHub may reject a token at any call.
  A failing credential is suspended **alone**: its repositories are not reviewed,
  reviews in flight wait, the `credential_invalid` event is raised, and every
  other credential keeps working. Replacing the token and restarting the service
  resumes the suspended repositories.

The startup check refuses to start, so a broken token cannot review with the wrong
login. The runtime check isolates the failure, so one expired machine-user token
does not stop the reviews of another owner.

### Expiry

GitHub reports a token's expiry date in the
`GitHub-Authentication-Token-Expiration` response header. The date is shown by
`autoreview validate` as `expires_at`, and `null` means the token has no expiry
date. Within `github.expiry_warning_days` of the expiry, the `credential_expiring`
event is raised once a day, so a token can be rotated before it stops working.

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
  organization." A credential with a fine-grained token therefore serves one owner
  or repository, which is why several owners need several credentials (see
  [repositories of several owners](#repositories-of-several-owners)).

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

Each owner can have its own credential, so one autoreview service reviews
repositories of several owners. A credential's `match` decides which of them it
serves, and the first match wins. Two ways to split the owners:

- **One token per owner** (fine-grained): each owner grants a token that covers
  that owner only. Each credential's `match` is `owner/*`.
- **One account per owner**: a credential for each owner names the account that
  should appear as the reviewer, for example a machine user for a public
  organization. See the worked example in [what a credential is](#worked-example).

A repository with its own token goes above its owner's credential. Every policy
rule needs a credential, and every credential needs a rule, so the two lists
match when `autoreview validate` passes.

## Approvals and branch protection

An approval counts toward a branch protection rule only when it comes from an
account with write permission on the repository, or from a designated code owner.
The protected branches page says: "You can require approving reviews from people
with write permissions in the repository or from a designated code owner." (see
[About protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches)).

The consequence for autoreview: the login of the credential that posts the
`APPROVE` review must have write permission on the repository, or the approval does
not count toward a required approval count. Give each credential's login write
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

- Put each token in the environment under the name given by its `token_env`. The
  name must start with `AUTOREVIEW_`. The install of the service sets the
  environment, see [install](../../install.md).
- Check that a token is set without printing it, using the name from
  `config.yaml`: `test -n "$AUTOREVIEW_GH_TOKEN_ZAJCA" && echo set`.
- Each `gh` child gets only the token of its credential, as `GH_TOKEN` in its own
  environment. No other child process receives a token.
- A token that is pasted into a chat, a log or a configuration file must be
  rotated. Rotating one credential does not affect the others. See
  [secrets](../safety/secrets.md).

## Related pages

- The configuration keys for credentials: [configuration: credentials](../../configuration.md#credentials).
- Who can open a review and how author classes work: [untrusted authors](../safety/untrusted-authors.md).
- Setting up a machine user: [solo maintainer](../guides/solo-maintainer.md).
- What a token can reach: [threat model](../safety/threat-model.md).
