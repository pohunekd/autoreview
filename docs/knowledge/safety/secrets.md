---
type: Safety
id: safety/secrets
title: Secrets
description: Where autoreview secrets live, which process may read each one, how to set and check them without printing values, what must stay out of config, policy and prompts, what a leaked GitHub token exposes, and how to rotate a leaked secret.
source_kind: manual
intents: [setup, help]
---

# Secrets

> **Status: 0.2.0.** This page describes autoreview 0.2.0. Parts
> marked planned or "not in v0.1" are not in 0.2.0. The container image this page
> mentions is not in 0.2.0 and is planned in #40.

autoreview has two kinds of secrets: the GitHub token of the
[credential](../concepts/github-identity.md) and the Matrix token of the
notifier. Both come from environment variables, or from the file given with
`--env-file`. The configuration files never contain a secret value, only the names
of the variables that hold them.

## Where each secret goes

| Secret | Variable | Read by | Never given to |
|---|---|---|---|
| GitHub token of the credential | The name in its `token_env` | `gh` | Reviewer sessions, the pohunek CLI, notifier backends |
| Backend secret | A name in an `*_env` key, for example `access_token_env` | That notifier backend | GitHub calls, reviewer sessions, other backends |

Each `gh` child gets the token as `GH_TOKEN` in its own
environment, and no other process that autoreview starts receives a GitHub token.

Every name in an `*_env` key, including `token_env`, must match `AUTOREVIEW_[A-Z0-9_]+`,
the rule of a name in the `--env-file`; any other name is `configuration_invalid`.

## What child processes receive

The `gh` and `pohunek` processes that autoreview starts inherit only the
variables of an allowlist from the service's environment, never one that starts
with `AUTOREVIEW_` or that an `*_env` key names:

- `gh`: `PATH`, `HOME`, `TMPDIR`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`,
  `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `GH_CONFIG_DIR`, the proxy variables
  `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY` (upper or lower case),
  `SSL_CERT_FILE`, `SSL_CERT_DIR`, `GH_TELEMETRY` and `DO_NOT_TRACK`.
  autoreview sets `GH_TOKEN` to the credential's token, `GH_PAGER=cat` and
  `GH_PROMPT_DISABLED=1`.
- `pohunek`: `PATH`, `HOME`, `TMPDIR`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`,
  `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_RUNTIME_DIR` and
  `POHUNEK_REMOTE_PORT`. autoreview sets `PAGER=cat`; the engine's calls also get
  `XDG_STATE_HOME` and `XDG_RUNTIME_DIR` from `pohunek.state_home` and
  `pohunek.runtime_dir`.

No other variable reaches a child: not a token of another tool in a shared
environment file, not `GITHUB_TOKEN`, `GH_HOST` or `GH_DEBUG`, and not the pohunek
session identity variables. A variable that gh or pohunek needs and that is not
on the list makes its call fail: `serve` refuses to start when its first `gh`
call, the token's login, fails, and a failed pohunek call fails as
`pohunek_request_failed`, with the CLI's error output as its cause, and is retried.

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

With the systemd units of install path b, the secrets sit in
`/etc/autoreview/autoreview.env`, which the units pass with `--env-file`:

- Owner `autoreview`, mode `0600`. The service reads the file itself, so the
  values never enter its environment. The installer creates the file empty and
  stops on another owner or mode.
- Contains only `NAME=value` lines for the `AUTOREVIEW_*` names.
- Stays out of repositories, shared dotfiles and any backup that leaves the host
  unencrypted.

With the container image, which is planned in #40, the same
file is passed through `env_file`. Keep it on the host with mode `0600`,
outside the mounted config directory, which is read-only and holds no secrets.

Write the values into the file with an editor. Do not put them on a command
line, in a shell history or in a script that prints them.

## The `--env-file`

The systemd units of install path b, or any process that starts autoreview with
an empty environment, pass the secrets in a file: `autoreview --env-file <path>
<command>`. Every command that reads the configuration takes the flag; `init` and
`profiles install` refuse it.

- The file must be a regular file owned by the user that runs autoreview, with no
  access for the group or others (mode `0600` or `0400`). A symlink is followed and
  the checks apply to the file it points to. A root-owned file fails the owner
  check of a service that runs as another user.
- Each line is `NAME=value`, where the name is `AUTOREVIEW_` followed by `A-Z`,
  `0-9` or `_`. The value is the rest of the line as written: quotes, `export` and
  escapes are not read. Blank lines and lines that start with `#` are skipped. A
  name may appear once.
- A name in the file wins over the same variable in the environment, also when its
  value is empty, which is refused as a missing secret. A name that the file lacks
  is read from the environment.
- The values only feed the `*_env` keys. They are never added to the environment of
  autoreview, so no child inherits them; `gh` gets its token as `GH_TOKEN` as
  described above.
- A refused file is `configuration_invalid`. The message names the path, and the
  mode, the owner or the line number; it never shows a line or a value.

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
  install` takes the host, the agent, the profile name, the model, the effort,
  the agent executable and the adapter limits; it takes no secret and writes an
  empty `[env]` table.
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
