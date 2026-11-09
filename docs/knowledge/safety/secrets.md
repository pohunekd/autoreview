---
type: Safety
id: safety/secrets
title: Secrets
description: Where autoreview secrets live, which process may read each one, how to set and check them without printing values, what must stay out of config, policy and prompts, what a leaked GitHub token exposes, and how to rotate a leaked secret.
source_kind: manual
intents: [setup, help]
---

# Secrets

> **Status: 0.1.0.** This page describes autoreview 0.1.0, the first release. Parts
> marked planned or "not in v0.1" are not in 0.1.0. The container image this page
> mentions comes after 0.1.0 and is planned in #40.

autoreview has two kinds of secrets: the GitHub token of the
[credential](../concepts/github-identity.md) and the Matrix token of the
notifier. Both come from environment variables. The
configuration files never contain a secret value, only the names of the variables
that hold them.

## Where each secret goes

| Secret | Variable | Read by | Never given to |
|---|---|---|---|
| GitHub token of the credential | The name in its `token_env` | `gh` | Reviewer sessions, the pohunek CLI, notifier backends |
| Backend secret | A name in an `*_env` key, for example `access_token_env` | That notifier backend | GitHub calls, reviewer sessions, other backends |

Each `gh` child gets the token as `GH_TOKEN` in its own
environment, and no other process that autoreview starts receives a GitHub token.

Every name in an `*_env` key, including `token_env`, must start with `AUTOREVIEW_`.
The `gh` and `pohunek` processes that autoreview starts inherit no variable that
starts with `AUTOREVIEW_` or that an `*_env` key names, no `GH_TOKEN` or
`MATRIX_ACCESS_TOKEN`, and no pohunek session identity variables. The `gh` child
gets the token set explicitly as `GH_TOKEN`.

The review agents are started by the pohunek daemon, not by autoreview, and run
with the daemon's environment, which autoreview does not control. Keep secrets
out of the daemon's environment and out of the pohunek profile.

## Blast radius of a token

The one token reaches what its own scope allows on GitHub; a leaked token
exposes every repository that scope covers (see
[GitHub identity](../concepts/github-identity.md#token-kinds-and-organization-approval)).
A token per owner or repository, which limits a leak to its repositories, is
planned in #13.

## The environment file

With the systemd units, the secrets sit in `/etc/autoreview/autoreview.env`:

- Owner `root`, mode `0600`. systemd reads the file as root before it drops
  privileges for the service.
- Contains only `KEY=value` lines for the `AUTOREVIEW_*` names.
- Stays out of repositories, shared dotfiles and any backup that leaves the host
  unencrypted.

With the container image, which comes after 0.1.0 (planned in #40), the same
file is passed through `env_file`. Keep it on the host with mode `0600`,
outside the mounted config directory, which is read-only and holds no secrets.

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
`token_env` name of the credential from `config.yaml`:

```sh
test -n "$AUTOREVIEW_GH_TOKEN" && echo set
test -n "$AUTOREVIEW_MATRIX_TOKEN" && echo set
```

To check the environment file of the service, load it into a shell that prints
nothing, then test the variable:

```sh
set -a; . /etc/autoreview/autoreview.env; set +a
test -n "$AUTOREVIEW_GH_TOKEN" && echo set
```

Do not paste the file's content into a ticket, a chat or a shell transcript.

## Rotate a leaked secret

Treat a secret as leaked when it was printed, committed, pasted or sent to a
place outside the host, or when you cannot tell who saw it.

1. Revoke the old value at its source: the GitHub token in the settings of the
   account that the credential names, the Matrix token on the homeserver.
2. Create a new value with the same scopes. Use the narrowest scopes the engine
   needs; see [GitHub identity](../concepts/github-identity.md).
3. Replace the value in `/etc/autoreview/autoreview.env`, keep mode `0600`, and
   restart the service: `sudo systemctl restart autoreview.service`.
4. Run `validate` and check the variable with `test -n` as above.
5. If the leak came from a file under version control, remove it from the
   history of that repository. Rotation comes first; history rewriting does not
   make a leaked secret safe.

## Related pages

- [Threat model](threat-model.md)
- [Configuration reference, environment variables](../../configuration.md#environment-variables)
- [Install, environment file](../../install.md#b-systemd-units)
- [GitHub identity](../concepts/github-identity.md)
