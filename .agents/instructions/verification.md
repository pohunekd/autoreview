# Verification

## Gate set

`scripts/gates` runs each gate, in the order below, and reports its own result.
With no argument it runs every group; CI runs the `docs` and `go` groups as two
jobs.

Docs group:

1. `check-docs` (`python3 -I scripts/check-docs`): relative links and their
   anchors, YAML code blocks, the status line of each page, forbidden words and
   emoji in `docs/` (never `docs/plan/`), and that every `examples/**/*.yaml`
   parses.

Go group, skipped with a reason while the repository has no `go.mod`:

2. `fmt`: `gofmt -l .` lists no file.
3. `vet`: `go vet ./...`
4. `staticcheck`: the version pinned in `scripts/gates`, run with `go run`.
5. `test`: `go test ./...`

Planned, not gates yet: the changelog check (`scripts/changelog check`, with
[changelog.md](changelog.md)) and validation of the example policies. Both land
with M1, which also decides how `validate` can run in CI without tokens or a
running pohunek daemon. Until then, the gate set above is the complete list.

Run `scripts/gates` on the final revision before every push and report each
gate's real result as printed. Never report a gate as passed when it was skipped
or its output was cut off. A skipped Go gate is reported as skipped, not passed.

## Testing policy

autoreview decides what is published on other people's pull requests. A bug in
that logic is silent and costly, so tests concentrate there and nowhere else.
Write a test only for:

- **Non-trivial pure logic** where a bug is silent and costly:
  - policy parsing and matching (first match wins, a rule that can never match is
    an error);
  - credential resolution (first match, `credential_not_covered`,
    `credential_unused`);
  - decision planning (request changes, comment, approve, or wait, from the
    findings and the author class);
  - incremental scope (what changed since the last published head) and the
    settling of earlier findings;
  - finding anchoring to changed lines;
  - Markdown neutralization of model output;
  - result protocol parsing for the Claude and Codex adapters;
  - scheduler and retry math (debounce, backoff, deadlines);
  - config validation (every key required, `*_env` names).
- **A fixed bug in that logic**, as a regression test that fails without the
  fix.
- **One behavioural scenario end to end** through the fake `Integrations`
  boundary: a candidate pull request is discovered, reviewed with a fake reviewer
  result, and published once. It is the only test that drives several packages
  together.

Do not write:

- tests per function, getters, trivial mappings, or anything the type system
  already guarantees;
- mocks of `gh`, of pohunek, or of GitHub. The `Integrations` fakes are the
  only test doubles; test logic on plain values (snapshots, event sequences,
  findings as data);
- tests for the wording of CLI output, documentation prose, or the prompts;
- coverage targets.

Delete a test that keeps breaking on intentional changes without ever having
caught a bug, and say so in the PR.

## Manual verification

Changes to discovery, reviewer runs, publication, notifications, or installed
files are verified by running the service against a real pohunek daemon and a
scratch GitHub repository that holds no private code. Use an example policy
from `examples/` and a token created for the scratch repository only.

The PR records, in words, the commands run, the pohunek version, and what was
observed: the review that was published, the decision, the logs' relevant
events. Screenshots are not applicable to this repository.

Manual verification is part of the issue that introduces the behaviour, not a
follow-up. A check that cannot be run is reported as unverified in the PR, with
the reason; it is never reported as passed.

## Published evidence

The repository is public. Command output, logs, PR text, and issue text must not
contain:

- any token, not even a prefix or a masked form;
- host names, overlay network addresses, personal paths, or session identifiers
  of the owner's machines;
- names or content of private repositories, or review text about them.

Use the scratch repository and placeholders such as `example/repo` in published
text. Describe checks against the owner's own daemons in words only.

## CI

`.github/workflows/ci.yml` finishes in a few minutes:

- `docs` runs `scripts/gates docs` with PyYAML pinned in a virtualenv.
- `go` runs `scripts/gates go`. Until `go.mod` exists the gates report
  themselves as skipped, so the job is green.
- `ci` is the only required status check. It fails unless every job succeeded.

When a workflow file changes, run `actionlint` at a pinned version on it and
report the result in the PR. It is not a gate yet.

Keep CI fast: before adding a job or a step, measure its cost on a PR run and
state it in the PR. A slow or flaky CI is a bug: file it.
