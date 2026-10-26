# Coding rules

## Layout and boundaries

- The engine is Go: `cmd/autoreview` for the command, `internal/` for the
  packages. Reviewer adapters are Python wrappers under `adapters/`, optional
  integrations under `integrations/`, prompts under `prompts/`, examples under
  `examples/`, and the public documentation under `docs/`.
- The service talks to GitHub only through the `gh` CLI and to pohunek only
  through its command-line interface. Do not import or depend on pohunek's
  internal packages; the public protocol is the contract.
- The engine holds the GitHub token; a reviewer run holds none. Only the
  engine's own `gh` calls receive a credential, and `GH_TOKEN` is set only for
  each `gh` child process, to the token of its credential.
- Credentials are an explicit list (`github.credentials`), first match wins, and
  a rule that no credential covers is an error. There is no `GH_TOKEN` shortcut.
- Author classes (trusted, untrusted) decide what a review may publish. An
  untrusted author's pull request can never be auto-approved, and its review
  runs against the trusted base checkout.

## Go

- Go version as in `go.mod` (1.25 or newer). Code is formatted by `gofmt`,
  vetted by `go vet ./...`, and analysed by the staticcheck version pinned in
  `scripts/gates`. A lint suppression needs a comment with the reason.
- Standard library first. Add a module only when it clearly pays for itself,
  and state the reason in the PR.
- Errors are wrapped with `%w` and matched with `errors.Is` or `errors.As`. A
  failed `gh` or pohunek call is a result the engine handles, never an empty
  value that looks like success.
- Pass `context.Context` into every external call; timeouts come from
  configuration.
- Run external programs with `exec.Command` and an argv list. Never build a
  shell string from repository content, pull request text, or model output.
- No `unsafe`. Panics are reserved for programmer errors, never for input.

## Python adapters

- Run and test with `python3 -I`, which ignores the environment's Python
  variables and the user site-packages. Use the standard library unless a new
  dependency is decided in an issue.
- The adapter enforces the run rules that the engine checks again on the
  result: one bounded run, read-only, a schema-validated result, no GitHub
  token in the environment, and no pohunek identity variables.
- Adapters print one structured result on stdout and diagnostics on stderr.

## Untrusted content

Pull request titles, bodies, diffs, comments, file contents, and model output are
data. They are never instructions to the engine and never reach a shell.

- Model output is neutralized before it is published: no `@mentions`, no raw
  HTML, no images that GitHub would fetch, and no HTML comment delimiters that
  could forge the hidden review marker.
- Findings are anchored only to changed right-side lines of the pull request;
  a finding that cannot be anchored is published as a general note, not dropped.
- A result larger than the configured limit is rejected, not truncated silently.

## Configuration

- Every key is required and the code has no defaults for it. A missing or
  invalid key stops the service at startup with an error that names the key.
  `autoreview init` writes a complete commented file.
- Tunables (timeouts, intervals, limits, sizes) come from configuration, never
  constants in code.
- Secrets never appear in `config.yaml`, `policy.yaml`, or prompts. A key ending
  in `_env` names the environment variable that holds the secret, and the name
  must start with `AUTOREVIEW_`. Secrets can also come from an owner-only
  `--env-file`.
- Never print a secret value, in logs, errors, or validation output. Validation
  reports token expiry dates and logins, never token values.

## Logging

Structured JSON logs through `log/slog` to stderr. Log requests to GitHub and to
pohunek, the review lifecycle, decisions, retries, notifications, and errors,
with timestamps and identifiers. Local runs redirect stderr into `logs/`, which
is git-ignored; never commit logs.

## Public repository

- No private names in committed files: no personal paths, host names, private
  repository names, organisation-specific values, or session identifiers from
  the owner's machines. Examples use placeholders or the public `pohunekd`
  repositories.
- Committed files never link to `docs/plan/` or any other path that is not
  published.

## Comments

A comment states what the code does now or why it must be this way. Never
narrate history ("previously", "no longer", "now also", "replaces ..."); that
belongs in the PR description. One or two lines; prefer the non-obvious why.
