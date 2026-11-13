---
name: gates
description: >-
  Run the autoreview gate set (documentation checks, the adapter test suites,
  then cargo fmt, clippy, test and the changelog check)
  locally and report each gate's real result. Use before any commit, push, or
  claim that a change is done.
---

# gates

Run `scripts/gates` from the repository root (or the worktree root). With no
argument it runs every group: `docs` (`check-docs`), `adapters` (one
`adapter-<name>` gate per `adapters/<name>/tests` directory), `rust` (`rust-fmt`,
`rust-clippy`, `rust-test`, `changelog`).
`scripts/gates docs`, `scripts/gates adapters` and `scripts/gates rust` run one
group. It prints a per-gate summary and exits non-zero if any gate failed.

- Read the summary, not a piped tail of the output: report each gate as
  `passed`, `skipped (reason)` or `failed (exit N)` exactly as printed.
- A formatting failure is fixed with `cargo fmt --all`; then rerun the gates.
- If `python3` cannot import `yaml`, the docs gate fails on the import, and the
  fix is to install PyYAML for that interpreter, not to skip the check.
- Never mark a gate passed that did not run, and never weaken a lint or test to
  get green.
