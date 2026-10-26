---
type: Safety
id: safety/secrets
title: Secrets
description: Where autoreview secrets live, which process may read each one, how to set and check them without printing values, what must stay out of config, policy and prompts, what a leaked GitHub token exposes, and how to rotate a leaked secret.
source_kind: manual
intents: [setup, help]
---

# Secrets

> **Status: planned for v0.1.** autoreview is not released yet. This page describes the design the first release implements; commands and keys may still change.

autoreview has two kinds of secrets: the GitHub tokens, one for each
[credential](../concepts/github-identity.md), and the backend secrets (for example
a Matrix token or a webhook URL). Both come from environment variables. The
configuration files never contain a secret value, only the names of the variables
that hold them.

## Where each secret goes

| Secret | Variable | Read by | Never given to |
|---|---|---|---|
| GitHub token of a credential | The name in its `token_env` | `gh`, for the repositories that credential serves | Reviewer sessions, the pohunek CLI, notifier backends, and any call for another credential's repositories |
| Backend secret | A name in an `*_env` key, for example `access_token_env` | That notifier backend | GitHub calls, reviewer sessions, other backends |

Each `gh` child gets only the token of its credential, as `GH_TOKEN` in its own
environment, and no other child process receives a GitHub token.

Every name in an `*_env` key, including `token_env`, must start with `AUTOREVIEW_`.
Child processes never inherit a GitHub token or pohunek session identity
variables. The reviewer's environment is cleaned before its session starts
(`cleanEnvironment`).

## Blast radius of a token

A token reaches only the repositories of its credential's `match`, and each `gh`
call gets only the one token that serves its repository. A leaked token therefore
exposes the repositories of its credential, not every repository the service
reviews. What the token can do on GitHub is still set by the token's own scope,
which the `match` does not change (see
[GitHub identity](../concepts/github-identity.md#token-kinds-and-organization-approval)).
Keep one variable per credential, so that a rotation touches only the token that
leaked.

## The environment file

With the systemd units, the secrets sit in `/etc/autoreview/autoreview.env`:

- Owner `root`, mode `0600`. systemd reads the file as root before it drops
  privileges for the service.
- Contains only `KEY=value` lines for the `AUTOREVIEW_*` names.
- Stays out of repositories, shared dotfiles and any backup that leaves the host
  unencrypted.

With the container, the same file is passed through `env_file`. Keep it on the
host with mode `0600`, outside the mounted config directory, which is read-only
and holds no secrets.

Write the values into the file with an editor. Do not put them on a command
line, in a shell history or in a script that prints them.

## Never in config, policy or prompts

- `config.yaml` names variables (`token_env`, `access_token_env`, `url_env`); it
  holds no value.
- `policy.yaml` holds rules and reviewer ids, no secret.
- Prompts are sent to the models and can appear in reviewer output. A secret in a
  prompt leaks to the model and to every review that reads it.

A secret written into `config.yaml` would sit in that file, in every copy of the
configuration directory and in its backups.

## Reviewer sessions hold no secrets

Reviewers run in pohunek sessions on review hosts, and they read untrusted
content. Keep the reviewer's environment free of secrets:

- **Do not put GitHub or backend secrets in a pohunek agent profile.** The
  profile's environment is secret-bearing when it has one. `autoreview profiles
  install` takes only the host, agent, model and profile name; it takes no secret.
- **Keep model credentials in the runtime's own login.** The `claude` or `codex`
  runtime uses the login it was set up with on the review host. Do not copy the
  credentials into the profile, the policy or the prompts.
- **Keep review hosts free of GitHub tokens.** The tokens belong to the
  autoreview host. The review host needs only the pohunek daemon and the
  runtime.
- Planned: adapters refuse known secret variables before a run. Not in v0.1;
  until then, the profile must not carry one.

## Check that a variable is set

Check without printing the value. Use `test`, never `echo` or `env`. Use the
`token_env` name of each credential from `config.yaml`:

```sh
test -n "$AUTOREVIEW_GH_TOKEN_ZAJCA" && echo set
test -n "$AUTOREVIEW_MATRIX_TOKEN" && echo set
```

To check the environment file of the service, load it into a shell that prints
nothing, then test the variable:

```sh
set -a; . /etc/autoreview/autoreview.env; set +a
test -n "$AUTOREVIEW_GH_TOKEN_ZAJCA" && echo set
```

Do not paste the file's content into a ticket, a chat or a shell transcript.

## Rotate a leaked secret

Treat a secret as leaked when it was printed, committed, pasted or sent to a
place outside the host, or when you cannot tell who saw it.

1. Revoke the old value at its source: the GitHub token in the settings of the
   account that the credential names, the Matrix token on the homeserver, the
   webhook URL in the chat service.
2. Create a new value with the same scopes. Use the narrowest scopes the engine
   needs; see [GitHub identity](../concepts/github-identity.md).
3. Replace the value in `/etc/autoreview/autoreview.env`, keep mode `0600`, and
   restart the service: `sudo systemctl restart autoreview.service`. Rotating one
   credential's token leaves the other credentials and their tokens unchanged.
4. Run `validate` and check the variable with `test -n` as above.
5. If the leak came from a file under version control, remove it from the
   history of that repository. Rotation comes first; history rewriting does not
   make a leaked secret safe.

## Related pages

- [Threat model](threat-model.md)
- [Configuration reference, environment variables](../../configuration.md#environment-variables)
- [Install, environment file](../../install.md#b-systemd-units)
- [GitHub identity](../concepts/github-identity.md)
