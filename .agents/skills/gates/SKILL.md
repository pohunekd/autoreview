---
name: gates
description: >-
  Run the autoreview gate set (documentation checks, then gofmt, go vet,
  staticcheck and go test once go.mod exists) locally and report each gate's
  real result. Use before any commit, push, or claim that a change is done.
---

# gates

Run `scripts/gates` from the repository root (or the worktree root). With no
argument it runs every group: `docs` (`check-docs`) and `go` (`fmt`, `vet`,
`staticcheck`, `test`). `scripts/gates docs` and `scripts/gates go` run one
group. It prints a per-gate summary and exits non-zero if any gate failed.

- Read the summary, not a piped tail of the output: report each gate as
  `passed`, `skipped (reason)` or `failed (exit N)` exactly as printed.
- A Go gate reported as skipped because there is no `go.mod` is not a pass; say
  so in the PR.
- A formatting failure is fixed with `gofmt -w` on the listed files; then rerun
  the gates.
- If `python3` cannot import `yaml`, the docs gate fails on the import, and the
  fix is to install PyYAML for that interpreter, not to skip the check.
- Never mark a gate passed that did not run, and never weaken a lint or test to
  get green.
